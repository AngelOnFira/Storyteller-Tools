use poise::serenity_prelude as serenity;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

struct Data {
    active_timer: Arc<Mutex<Option<JoinHandle<()>>>>,
}
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Debug, poise::ChoiceParameter)]
enum GatherDuration {
    #[name = "5 minutes"]
    FiveMinutes,
    #[name = "10 minutes"]
    TenMinutes,
    #[name = "Custom"]
    Custom,
    #[name = "Test (1 min, no ping)"]
    Test,
}

/// Update the voice channel status on all voice channels in a guild.
async fn update_voice_channel_statuses(
    http: &serenity::Http,
    guild_id: serenity::GuildId,
    status_text: &str,
) {
    let channels = match guild_id.channels(http).await {
        Ok(c) => c,
        Err(e) => {
            error!(guild_id = %guild_id, error = %e, "failed to fetch guild channels");
            return;
        }
    };

    let edits = channels.into_iter().filter_map(|(_id, mut channel)| {
        if channel.kind != serenity::model::channel::ChannelType::Voice {
            return None;
        }
        Some(async move {
            let builder = serenity::builder::EditChannel::new().status(status_text);
            match channel.edit(http, builder).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    warn!(guild_id = %guild_id, channel_name = %channel.name, channel_id = %channel.id, error = %e, "failed to update voice channel status");
                    Err(())
                }
            }
        })
    });

    let results = futures::future::join_all(edits).await;
    let updated = results.iter().filter(|r| r.is_ok()).count() as u32;
    info!(guild_id = %guild_id, channels_updated = updated, status_text = %status_text, "updated voice channel statuses");
}

/// Check if the invoking user has a role named "storyteller" (case-insensitive).
async fn has_storyteller_role(ctx: Context<'_>) -> Result<bool, Error> {
    let guild_id = match ctx.guild_id() {
        Some(id) => id,
        None => return Ok(false),
    };

    let roles = guild_id.roles(ctx.http()).await?;
    let member = ctx.author_member().await;
    let member = match member {
        Some(m) => m,
        None => return Ok(false),
    };

    for role_id in &member.roles {
        if let Some(role) = roles.get(role_id) {
            if role.name.eq_ignore_ascii_case("storyteller") {
                info!(guild_id = %guild_id, user = %ctx.author().name, user_id = %ctx.author().id, "role check passed: user has storyteller role");
                return Ok(true);
            }
        }
    }
    warn!(guild_id = %guild_id, user = %ctx.author().name, user_id = %ctx.author().id, "role check failed: user lacks storyteller role");
    Ok(false)
}

/// Send an ephemeral reply only the command user can see.
async fn reply_ephemeral(ctx: Context<'_>, content: &str) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Call everyone back to Town Square with a countdown timer.
#[poise::command(slash_command, guild_only)]
async fn gather(
    ctx: Context<'_>,
    #[description = "How long until gathering"] duration: GatherDuration,
    #[description = "Custom duration in minutes (1-60)"] custom_minutes: Option<u64>,
) -> Result<(), Error> {
    info!(user = %ctx.author().name, user_id = %ctx.author().id, guild_id = ?ctx.guild_id(), duration = ?duration, custom_minutes = ?custom_minutes, "/gather command invoked");

    if !has_storyteller_role(ctx).await? {
        reply_ephemeral(
            ctx,
            "You need the **Storyteller** role to use this command.",
        )
        .await?;
        return Ok(());
    }

    let minutes: u64 = match duration {
        GatherDuration::FiveMinutes => 5,
        GatherDuration::TenMinutes => 10,
        GatherDuration::Test => 1,
        GatherDuration::Custom => match custom_minutes {
            Some(m) if (1..=60).contains(&m) => m,
            Some(m) => {
                warn!(user = %ctx.author().name, custom_minutes = m, "invalid custom duration");
                reply_ephemeral(ctx, "Custom duration must be between 1 and 60 minutes.").await?;
                return Ok(());
            }
            None => {
                warn!(user = %ctx.author().name, "custom duration selected but custom_minutes not provided");
                reply_ephemeral(
                    ctx,
                    "Please provide `custom_minutes` when using Custom duration.",
                )
                .await?;
                return Ok(());
            }
        },
    };

    let is_test = matches!(duration, GatherDuration::Test);
    info!(
        minutes = minutes,
        is_test = is_test,
        "resolved gather duration"
    );

    // Cancel any existing timer
    {
        let mut timer = ctx.data().active_timer.lock().await;
        if let Some(handle) = timer.take() {
            handle.abort();
            info!("cancelled existing timer (replaced by new /gather)");
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let total_duration = std::time::Duration::from_secs(minutes * 60);
    let deadline = tokio::time::Instant::now() + total_duration;
    let end_timestamp = now + minutes * 60;

    // Send visible message with Discord's live-updating relative timestamp
    let mode_note = if is_test {
        " *(test mode — no ping)*"
    } else {
        ""
    };
    let channel_id = ctx.channel_id();
    let http = ctx.serenity_context().http.clone();
    let countdown_msg = channel_id
        .say(
            &http,
            format!("Return to **Town Square** <t:{end_timestamp}:R>{mode_note}"),
        )
        .await?;
    let countdown_msg_id = countdown_msg.id;
    info!(channel_id = %channel_id, message_id = %countdown_msg_id, end_timestamp = end_timestamp, "sent countdown message");

    // Reply to the storyteller with ephemeral confirmation
    reply_ephemeral(ctx, &format!("Timer started for {minutes} minute(s).")).await?;

    // Set initial voice channel statuses with timestamp
    let guild_id = ctx.guild_id().unwrap();
    update_voice_channel_statuses(&http, guild_id, &format!("Return <t:{end_timestamp}:R>")).await;

    // Spawn background countdown task
    let active_timer = ctx.data().active_timer.clone();
    let http2 = http.clone();

    let handle = tokio::spawn(async move {
        info!(guild_id = %guild_id, total_seconds = minutes * 60, "background timer started");

        tokio::time::sleep_until(deadline).await;
        info!(guild_id = %guild_id, channel_id = %channel_id, is_test = is_test, "timer expired, sending final message");

        // Send final message first so the ping feels on time
        if is_test {
            let _ = channel_id
                .say(
                    &http2,
                    "Time's up! Everyone back to **Town Square**! *(test mode — no ping)*",
                )
                .await;
        } else {
            let _ = channel_id
                .say(
                    &http2,
                    "<@&1483859041196310570> Time's up! Everyone back to **Town Square**!",
                )
                .await;
        }

        // Then clean up: delete the old countdown message and clear statuses
        match channel_id.delete_message(&http2, countdown_msg_id).await {
            Ok(_) => info!(message_id = %countdown_msg_id, "deleted countdown message"),
            Err(e) => {
                warn!(message_id = %countdown_msg_id, error = %e, "failed to delete countdown message")
            }
        }
        update_voice_channel_statuses(&http2, guild_id, "").await;
        info!(guild_id = %guild_id, "cleared voice channel statuses");

        // Clean up handle
        let mut timer = active_timer.lock().await;
        *timer = None;
        info!(guild_id = %guild_id, "timer complete, handle cleaned up");
    });

    // Store the handle
    {
        let mut timer = ctx.data().active_timer.lock().await;
        *timer = Some(handle);
    }
    info!(guild_id = %guild_id, "timer handle stored");

    Ok(())
}

/// Cancel the active gather timer.
#[poise::command(slash_command, guild_only)]
async fn cancel(ctx: Context<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().name, user_id = %ctx.author().id, guild_id = ?ctx.guild_id(), "/cancel command invoked");

    if !has_storyteller_role(ctx).await? {
        reply_ephemeral(
            ctx,
            "You need the **Storyteller** role to use this command.",
        )
        .await?;
        return Ok(());
    }

    let mut timer = ctx.data().active_timer.lock().await;
    if let Some(handle) = timer.take() {
        handle.abort();
        drop(timer); // release lock before async work

        let guild_id = ctx.guild_id().unwrap();
        let http = ctx.serenity_context().http.clone();
        update_voice_channel_statuses(&http, guild_id, "").await;

        info!(guild_id = %guild_id, user = %ctx.author().name, "timer cancelled via /cancel, voice statuses cleared");
        reply_ephemeral(ctx, "Timer cancelled, voice channel statuses cleared.").await?;
    } else {
        info!(user = %ctx.author().name, "no active timer to cancel");
        reply_ephemeral(ctx, "No active timer to cancel.").await?;
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN env var not set");
    info!("starting bot");

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![gather(), cancel()],
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                info!(bot_user = %ready.user.name, bot_id = %ready.user.id, "bot is ready, commands registered globally");
                Ok(Data {
                    active_timer: Arc::new(Mutex::new(None)),
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::non_privileged();
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("Failed to create client");

    if let Err(e) = client.start().await {
        error!(error = %e, "client error");
    }
}
