# Populate environment variables
cat <<EOF > .env
DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/your-id/your-token"
RESEND_API_KEY="re_123456789"
CORPORATE_ALERT_EMAIL="treasury@acme-corp.com"
PROGRAM_ID="<DEPLOYED_PROGRAM_ID>"
SOLANA_RPC_URL="https://api.devnet.solana.com"
EOF

# Launch turnkey worker via docker-compose
docker compose up -d --build

# Confirm telemetry output & liveness
curl -I http://localhost:8080/healthz
docker compose logs -f aegis-telemetry-worker