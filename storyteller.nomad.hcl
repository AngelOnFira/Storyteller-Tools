job "storyteller" {
  type        = "service"
  datacenters = ["dc1"]

  group "bot" {
    count = 1

    network {
      mode = "bridge"
    }

    restart {
      attempts = 5
      interval = "30m"
      delay    = "15s"
      mode     = "delay"
    }

    task "storyteller" {
      driver = "docker"

      config {
        image = "ghcr.io/angelonfira/storyteller-tools:latest"
      }

      template {
        destination = "${NOMAD_SECRETS_DIR}/discord.env"
        env         = true
        change_mode = "restart"
        data        = <<EOT
DISCORD_TOKEN={{ with nomadVar "nomad/jobs/storyteller" }}{{ .discord_token }}{{ end }}
EOT
      }

      resources {
        cpu    = 128
        memory = 128
      }
    }
  }

  update {
    max_parallel     = 1
    min_healthy_time = "10s"
    healthy_deadline = "3m"
    auto_revert      = true
  }
}
