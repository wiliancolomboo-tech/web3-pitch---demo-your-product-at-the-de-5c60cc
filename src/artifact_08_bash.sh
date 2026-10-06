# Set Solana CLI target
solana config set --url https://api.devnet.solana.com

# Build Anchor Program with deterministic bytecode
anchor build

# Deploy Program to Solana Devnet
anchor deploy --program-name aegis_stream --provider.cluster devnet

# Verify deployment
solana program show <PROGRAM_ID>