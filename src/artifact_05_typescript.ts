// tests/aegis-stream.ts
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { AegisStream } from "../target/types/aegis_stream";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { assert } from "chai";

describe("AegisStream Protocol Integration Tests", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.AegisStream as Program<AegisStream>;

  let mint: anchor.web3.PublicKey;
  let depositorTokenAccount: anchor.web3.PublicKey;
  let beneficiaryTokenAccount: anchor.web3.PublicKey;
  let depositorRefundTokenAccount: anchor.web3.PublicKey;

  const depositor = anchor.web3.Keypair.generate();
  const beneficiary = anchor.web3.Keypair.generate();
  const arbiter = anchor.web3.Keypair.generate();

  let vaultStatePda: anchor.web3.PublicKey;
  let vaultAuthorityPda: anchor.web3.PublicKey;
  let vaultTokenAccountPda: anchor.web3.PublicKey;

  const depositAmount = new anchor.BN(1_000_000_000); // 1,000 tokens (decimals = 6)
  const streamRate = new anchor.BN(10_000_000); // 10 tokens/sec

  before(async () => {
    // Fund test wallets with SOL
    for (const kp of [depositor, beneficiary, arbiter]) {
      const sig = await provider.connection.requestAirdrop(
        kp.publicKey,
        5 * anchor.web3.LAMPORTS_PER_SOL
      );
      await provider.connection.confirmTransaction(sig);
    }

    // Create Mock SPL Mint
    mint = await createMint(
      provider.connection,
      depositor,
      depositor.publicKey,
      null,
      6
    );

    depositorTokenAccount = await createAccount(
      provider.connection,
      depositor,
      mint,
      depositor.publicKey
    );

    beneficiaryTokenAccount = await createAccount(
      provider.connection,
      depositor,
      mint,
      beneficiary.publicKey
    );

    // Mint tokens to depositor
    await mintTo(
      provider.connection,
      depositor,
      mint,
      depositorTokenAccount,
      depositor,
      10_000_000_000
    );

    // Find PDAs
    [vaultStatePda] = anchor.web3.PublicKey.findProgramAddressSync(
      [
        Buffer.from("aegis_vault"),
        depositor.publicKey.toBuffer(),
        beneficiary.publicKey.toBuffer(),
      ],
      program.programId
    );

    [vaultAuthorityPda] = anchor.web3.PublicKey.findProgramAddressSync(
      [
        Buffer.from("vault_authority"),
        depositor.publicKey.toBuffer(),
        beneficiary.publicKey.toBuffer(),
      ],
      program.programId
    );

    [vaultTokenAccountPda] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("token_escrow"), vaultStatePda.toBuffer()],
      program.programId
    );
  });

  it("Initializes the institutional streaming vault successfully", async () => {
    const now = Math.floor(Date.now() / 1000);
    const cliff = new anchor.BN(now + 2); // 2 second cliff
    const end = new anchor.BN(now + 102);  // 100s stream duration

    await program.methods
      .initializeVault(streamRate, cliff, end, depositAmount)
      .accounts({
        depositor: depositor.publicKey,
        beneficiary: beneficiary.publicKey,
        arbiter: arbiter.publicKey,
        mint: mint,
        vaultState: vaultStatePda,
        vaultAuthority: vaultAuthorityPda,
        vaultTokenAccount: vaultTokenAccountPda,
        depositorTokenAccount: depositorTokenAccount,
        systemProgram: anchor.web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .signers([depositor])
      .rpc();

    const vaultAccount = await program.account.vaultState.fetch(vaultStatePda);
    assert.ok(vaultAccount.depositor.equals(depositor.publicKey));
    assert.ok(vaultAccount.beneficiary.equals(beneficiary.publicKey));
    assert.strictEqual(vaultAccount.totalDeposited.toString(), depositAmount.toString());
    assert.deepStrictEqual(vaultAccount.status, { active: {} });

    const escrowToken = await getAccount(provider.connection, vaultTokenAccountPda);
    assert.strictEqual(escrowToken.amount.toString(), depositAmount.toString());
  });

  it("Rejects premature stream claim before cliff timestamp", async () => {
    try {
      await program.methods
        .executeRelease()
        .accounts({
          caller: beneficiary.publicKey,
          vaultState: vaultStatePda,
          vaultAuthority: vaultAuthorityPda,
          vaultTokenAccount: vaultTokenAccountPda,
          beneficiaryTokenAccount: beneficiaryTokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([beneficiary])
        .rpc();
      assert.fail("Expected failure before cliff timestamp");
    } catch (err: any) {
      assert.include(err.toString(), "CliffNotReached");
    }
  });

  it("Executes milestone release after cliff maturation", async () => {
    // Wait 3.5 seconds to pass cliff and accrue claimable streaming tokens
    await new Promise((resolve) => setTimeout(resolve, 3500));

    const initialBeneficiaryBalance = (
      await getAccount(provider.connection, beneficiaryTokenAccount)
    ).amount;

    await program.methods
      .executeRelease()
      .accounts({
        caller: beneficiary.publicKey,
        vaultState: vaultStatePda,
        vaultAuthority: vaultAuthorityPda,
        vaultTokenAccount: vaultTokenAccountPda,
        beneficiaryTokenAccount: beneficiaryTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([beneficiary])
      .rpc();

    const finalBeneficiaryBalance = (
      await getAccount(provider.connection, beneficiaryTokenAccount)
    ).amount;
    assert.isTrue(finalBeneficiaryBalance > initialBeneficiaryBalance);
  });

  it("Enforces Arbiter Dispute Escalation and Multi-party Split Resolution", async () => {
    // Arbiter triggers dispute
    await program.methods
      .triggerDispute()
      .accounts({
        signer: arbiter.publicKey,
        vaultState: vaultStatePda,
      })
      .signers([arbiter])
      .rpc();

    let vaultAccount = await program.account.vaultState.fetch(vaultStatePda);
    assert.deepStrictEqual(vaultAccount.status, { disputed: {} });

    // Dispute Resolution: 60% to beneficiary, 40% refund to depositor (6000 bps)
    await program.methods
      .resolveDispute(6000)
      .accounts({
        arbiter: arbiter.publicKey,
        vaultState: vaultStatePda,
        vaultAuthority: vaultAuthorityPda,
        vaultTokenAccount: vaultTokenAccountPda,
        beneficiaryTokenAccount: beneficiaryTokenAccount,
        depositorTokenAccount: depositorTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([arbiter])
      .rpc();

    vaultAccount = await program.account.vaultState.fetch(vaultStatePda);
    assert.deepStrictEqual(vaultAccount.status, { cancelled: {} });

    const remainingEscrow = await getAccount(provider.connection, vaultTokenAccountPda);
    assert.strictEqual(remainingEscrow.amount.toString(), "0");
  });
});