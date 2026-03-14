FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --no-create-home --shell /bin/false bot
COPY discord-bot-storyteller /usr/local/bin/discord-bot-storyteller
USER bot
ENTRYPOINT ["/usr/local/bin/discord-bot-storyteller"]
