// src/orchestrator.ts
import { Connection, PublicKey, Keypair, Commitment } from "@solana/web3.js";
import { Program, AnchorProvider, Wallet } from "@coral-xyz/anchor";
import axios from "axios";
import { Resend } from "resend";
import * as http from "http";

// Interface Definitions
interface Config {
  rpcUrl: string;
  wsUrl: string;
  programId: string;
  keeperPrivateKey: string;
  discordWebhookUrl: string;
  resendApiKey: string;
  corporateAlertEmail: string;
  port: number;
}

interface VaultEventNotification {
  type: "INITIALIZED" | "RELEASED" | "DISPUTED" | "RESOLVED";
  vaultPubkey: string;
  signature: string;
  metadata: Record<string, any>;
}

export class AegisEnterpriseEngine {
  private connection: Connection;
  private resend: Resend;
  private config: Config;
  private isHealthy: boolean = true;
  private processedSignatures: Set<string> = new Set();

  constructor(config: Config) {
    this.config = config;
    const commitment: Commitment = "confirmed";
    this.connection = new Connection(config.rpcUrl, {
      wsEndpoint: config.wsUrl,
      commitment,
    });
    this.resend = new Resend(config.resendApiKey);
  }

  public async start(): Promise<void> {
    console.log("[AEGIS] Starting Enterprise Event Telemetry Engine...");
    this.startHealthCheckServer();
    this.subscribeToContractEvents();
  }

  private startHealthCheckServer(): void {
    const server = http.createServer((req, res) => {
      if (req.url === "/healthz") {
        if (this.isHealthy) {
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ status: "healthy", timestamp: Date.now() }));
        } else {
          res.writeHead(503, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ status: "unhealthy" }));
        }
      } else {
        res.writeHead(404);
        res.end();
      }
    });

    server.listen(this.config.port, () => {
      console.log(`[AEGIS] Telemetry HTTP healthcheck listening on :${this.config.port}`);
    });
  }

  private subscribeToContractEvents(): void {
    const programPubkey = new PublicKey(this.config.programId);
    console.log(`[AEGIS] Subscribing to logs for program: ${programPubkey.toBase58()}`);

    this.connection.onLogs(
      programPubkey,
      async ({ logs, err, signature }) => {
        if (err || this.processedSignatures.has(signature)) return;
        this.processedSignatures.add(signature);

        // Keep set size bounded
        if (this.processedSignatures.size > 10_000) {
          const firstItem = this.processedSignatures.values().next().value;
          if (firstItem) this.processedSignatures.delete(firstItem);
        }

        for (const log of logs) {
          if (log.includes("VaultInitializedEvent")) {
            await this.handleNotification({
              type: "INITIALIZED",
              vaultPubkey: programPubkey.toBase58(),
              signature,
              metadata: { raw: log },
            });
          } else if (log.includes("MilestoneReleasedEvent")) {
            await this.handleNotification({
              type: "RELEASED",
              vaultPubkey: programPubkey.toBase58(),
              signature,
              metadata: { raw: log },
            });
          } else if (log.includes("DisputeOpenedEvent")) {
            await this.handleNotification({
              type: "DISPUTED",
              vaultPubkey: programPubkey.toBase58(),
              signature,
              metadata: { severity: "CRITICAL", raw: log },
            });
          }
        }
      },
      "confirmed"
    );
  }

  private async handleNotification(evt: VaultEventNotification): Promise<void> {
    console.log(`[EVENT] Received: ${evt.type} | Sig: ${evt.signature}`);
    await Promise.all([
      this.sendDiscordWebhook(evt),
      this.sendResendEmail(evt),
    ]);
  }

  private async sendDiscordWebhook(evt: VaultEventNotification): Promise<void> {
    if (!this.config.discordWebhookUrl) return;

    const colors = {
      INITIALIZED: 0x3498db, // Blue
      RELEASED: 0x2ecc71,    // Green
      DISPUTED: 0xe74c3c,    // Red
      RESOLVED: 0xf1c40f,    // Yellow
    };

    const payload = {
      username: "AegisStream Sentinel",
      embeds: [
        {
          title: `🛡️ AegisStream Protocol Alert: ${evt.type}`,
          color: colors[evt.type] || 0xffffff,
          fields: [
            { name: "Transaction Signature", value: `\`${evt.signature}\``, inline: false },
            { name: "Program / Target", value: `\`${evt.vaultPubkey}\``, inline: true },
            { name: "Explorer Link", value: `[Solana Explorer](https://explorer.solana.com/tx/${evt.signature}?cluster=devnet)`, inline: true },
          ],
          timestamp: new Date().toISOString(),
          footer: { text: "AegisStream Security Attestation Service" },
        },
      ],
    };

    try {
      await axios.post(this.config.discordWebhookUrl, payload, { timeout: 5000 });
    } catch (err: any) {
      console.error("[AEGIS-DISCORD] Webhook delivery failed:", err.message);
    }
  }

  private async sendResendEmail(evt: VaultEventNotification): Promise<void> {
    if (!this.config.resendApiKey || !this.config.corporateAlertEmail) return;

    try {
      await this.resend.emails.send({
        from: "alerts@aegisstream.io",
        to: this.config.corporateAlertEmail,
        subject: `[AEGIS-ALERT] ${evt.type} - Audit Signature ${evt.signature.slice(0, 8)}`,
        html: `
          <h2>AegisStream Real-Time State Attestation</h2>
          <p>An on-chain state transition was confirmed by the consensus engine.</p>
          <ul>
            <li><strong>Event Type:</strong> ${evt.type}</li>
            <li><strong>Transaction Signature:</strong> <code>${evt.signature}</code></li>
            <li><strong>Timestamp:</strong> ${new Date().toISOString()}</li>
          </ul>
          <p><a href="https://explorer.solana.com/tx/${evt.signature}?cluster=devnet">View on Solana Explorer</a></p>
        `,
      });
    } catch (err: any) {
      console.error("[AEGIS-RESEND] Email delivery failed:", err.message);
    }
  }
}

// Direct Execution Entrypoint
if (require.main === module) {
  const config: Config = {
    rpcUrl: process.env.SOLANA_RPC_URL || "https://api.devnet.solana.com",
    wsUrl: process.env.SOLANA_WS_URL || "wss://api.devnet.solana.com",
    programId: process.env.PROGRAM_ID || "Aeg1sStream11111111111111111111111111111111",
    keeperPrivateKey: process.env.KEEPER_PRIVATE_KEY || "",
    discordWebhookUrl: process.env.DISCORD_WEBHOOK_URL || "",
    resendApiKey: process.env.RESEND_API_KEY || "",
    corporateAlertEmail: process.env.CORPORATE_ALERT_EMAIL || "treasury@enterprise.io",
    port: parseInt(process.env.PORT || "8080", 10),
  };

  const engine = new AegisEnterpriseEngine(config);
  engine.start().catch((err) => {
    console.error("[FATAL] Aegis Engine startup error:", err);
    process.exit(1);
  });
}