use poise::serenity_prelude as serenity;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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
            eprintln!("Failed to fetch channels: {e}");
            return;
        }
    };

    for (_id, mut channel) in channels {
        if channel.kind == serenity::model::channel::ChannelType::Voice {
            let builder = serenity::builder::EditChannel::new().status(status_text);
            if let Err(e) = channel.edit(http, builder).await {
                eprintln!("Failed to update status on channel {}: {e}", channel.name);
            }
        }
    }
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
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Send an ephemeral reply only the command user can see.
async fn reply_ephemeral(ctx: Context<'_>, content: &str) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().content(content).ephemeral(true)).await?;
    Ok(())
}

/// Call everyone back to Town Square with a countdown timer.
#[poise::command(slash_command, guild_only)]
async fn gather(
    ctx: Context<'_>,
    #[description = "How long until gathering"] duration: GatherDuration,
    #[description = "Custom duration in minutes (1-60)"] custom_minutes: Option<u64>,
) -> Result<(), Error> {
    if !has_storyteller_role(ctx).await? {
        reply_ephemeral(ctx, "You need the **Storyteller** role to use this command.").await?;
        return Ok(());
    }

    let minutes: u64 = match duration {
        GatherDuration::FiveMinutes => 5,
        GatherDuration::TenMinutes => 10,
        GatherDuration::Test => 1,
        GatherDuration::Custom => match custom_minutes {
            Some(m) if (1..=60).contains(&m) => m,
            Some(_) => {
                reply_ephemeral(ctx, "Custom duration must be between 1 and 60 minutes.").await?;
                return Ok(());
            }
            None => {
                reply_ephemeral(ctx, "Please provide `custom_minutes` when using Custom duration.").await?;
                return Ok(());
            }
        },
    };

    let is_test = matches!(duration, GatherDuration::Test);

    // Cancel any existing timer
    {
        let mut timer = ctx.data().active_timer.lock().await;
        if let Some(handle) = timer.take() {
            handle.abort();
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let end_timestamp = now + minutes * 60;

    // Send visible message with Discord's live-updating relative timestamp
    let mode_note = if is_test { " *(test mode — no @everyone ping)*" } else { "" };
    let channel_id = ctx.channel_id();
    let http = ctx.serenity_context().http.clone();
    let countdown_msg = channel_id
        .say(&http, format!(
            "Return to **Town Square** <t:{end_timestamp}:R>{mode_note}"
        ))
        .await?;
    let countdown_msg_id = countdown_msg.id;

    // Reply to the storyteller with ephemeral confirmation
    reply_ephemeral(ctx, &format!("Timer started for {minutes} minute(s).")).await?;

    // Set initial voice channel statuses with timestamp
    let guild_id = ctx.guild_id().unwrap();
    update_voice_channel_statuses(
        &http,
        guild_id,
        &format!("Return <t:{end_timestamp}:R>"),
    )
    .await;

    // Spawn background countdown task
    let active_timer = ctx.data().active_timer.clone();
    let http2 = http.clone();

    let handle = tokio::spawn(async move {
        let total_seconds = minutes * 60;
        let mut elapsed: u64 = 0;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            elapsed += 60;

            if elapsed >= total_seconds {
                // Send final message first so the ping feels on time
                if is_test {
                    let _ = channel_id
                        .say(&http2, "Time's up! Everyone back to **Town Square**! *(test mode — no ping)*")
                        .await;
                } else {
                    let _ = channel_id
                        .say(&http2, "@everyone Time's up! Everyone back to **Town Square**!")
                        .await;
                }

                // Then clean up: delete the old countdown message and clear statuses
                let _ = channel_id.delete_message(&http2, countdown_msg_id).await;
                update_voice_channel_statuses(&http2, guild_id, "").await;

                // Clean up handle
                let mut timer = active_timer.lock().await;
                *timer = None;
                return;
            }

            // Update voice channel statuses with remaining time (plain text fallback)
            let remaining_secs = total_seconds - elapsed;
            let remaining_mins = (remaining_secs + 59) / 60; // round up
            update_voice_channel_statuses(
                &http2,
                guild_id,
                &format!("Return to Town Square in {remaining_mins} min"),
            )
            .await;
        }
    });

    // Store the handle
    {
        let mut timer = ctx.data().active_timer.lock().await;
        *timer = Some(handle);
    }

    Ok(())
}

/// Cancel the active gather timer.
#[poise::command(slash_command, guild_only)]
async fn cancel(ctx: Context<'_>) -> Result<(), Error> {
    if !has_storyteller_role(ctx).await? {
        reply_ephemeral(ctx, "You need the **Storyteller** role to use this command.").await?;
        return Ok(());
    }

    let mut timer = ctx.data().active_timer.lock().await;
    if let Some(handle) = timer.take() {
        handle.abort();
        drop(timer); // release lock before async work

        let guild_id = ctx.guild_id().unwrap();
        let http = ctx.serenity_context().http.clone();
        update_voice_channel_statuses(&http, guild_id, "").await;

        reply_ephemeral(ctx, "Timer cancelled, voice channel statuses cleared.").await?;
    } else {
        reply_ephemeral(ctx, "No active timer to cancel.").await?;
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN env var not set");

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![gather(), cancel()],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                println!("Bot is ready!");
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
        eprintln!("Client error: {e}");
    }
}
