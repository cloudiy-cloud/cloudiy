import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, SystemProgram } from "@solana/web3.js";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  getAssociatedTokenAddressSync,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import * as nacl from "tweetnacl";
import { CloudiyEscrow } from "../target/types/cloudiy_escrow";
import * as h from "./helpers";

const AMOUNT = 1_000_000; // 1 test token (6 decimals)
const FEE_BPS = 400;
const feeOf = (a: number) => Math.floor((a * FEE_BPS) / 10_000);

describe("cloudiy-escrow: core flows", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.CloudiyEscrow as Program<CloudiyEscrow>;
  const connection = provider.connection;
  const payer = (provider.wallet as anchor.Wallet).payer;

  let mint: PublicKey;
  let providerWallet: Keypair; // payout destination
  let consumerToken: PublicKey;
  let providerToken: PublicKey;
  let feeToken: PublicKey;

  before(async () => {
    await h.airdrop(connection, payer.publicKey, 10);
    providerWallet = h.fundedKeypair();

    mint = await createMint(connection, payer, payer.publicKey, null, 6);
    consumerToken = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, payer.publicKey)
    ).address;
    providerToken = (
      await getOrCreateAssociatedTokenAccount(
        connection,
        payer,
        mint,
        providerWallet.publicKey
      )
    ).address;
    feeToken = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, h.FEE_AUTHORITY)
    ).address;

    await mintTo(connection, payer, mint, consumerToken, payer, 1_000_000_000);
  });

  // Fund a fresh escrow. Consumer is always the provider wallet (payer) so it
  // has SOL + tokens; returns the job/vault PDAs and the node keypair.
  async function fundJob(nodePub: Uint8Array, timeout = 600) {
    const jobId = h.randomJobId();
    const [job] = h.jobPda(program.programId, payer.publicKey, jobId);
    const [vault] = h.vaultPda(program.programId, job);
    await program.methods
      .createJob(Array.from(jobId), new anchor.BN(AMOUNT), new anchor.BN(timeout), Array.from(nodePub))
      .accounts({
        consumer: payer.publicKey,
        provider: providerWallet.publicKey,
        mint,
        job,
        vault,
        consumerToken,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    return { jobId, job, vault };
  }

  it("create_job locks funds in the vault", async () => {
    const node = nacl.sign.keyPair();
    const { vault } = await fundJob(node.publicKey);
    const bal = await getAccount(connection, vault);
    assert.equal(Number(bal.amount), AMOUNT);
  });

  it("release (trusted) pays provider minus fee and closes the job", async () => {
    const node = nacl.sign.keyPair();
    const { job, vault } = await fundJob(node.publicKey);
    const before = Number((await getAccount(connection, providerToken)).amount);

    await program.methods
      .release()
      .accounts({
        consumer: payer.publicKey,
        job,
        vault,
        providerToken,
        feeToken,
        storageToken: null,
        authorToken: null,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();

    const after = Number((await getAccount(connection, providerToken)).amount);
    assert.equal(after - before, AMOUNT - feeOf(AMOUNT));
    // Job + vault closed.
    assert.isNull(await connection.getAccountInfo(job));
    assert.isNull(await connection.getAccountInfo(vault));
  });

  it("release_verified pays out with a valid proof — settler != consumer (A3)", async () => {
    const node = nacl.sign.keyPair();
    const { jobId, job, vault } = await fundJob(node.publicKey);

    const settler = h.fundedKeypair();
    await h.airdrop(connection, settler.publicKey, 2);

    const output = Buffer.from("real-output");
    const msg = h.resultMessage(jobId, output);
    const sig = nacl.sign.detached(msg, node.secretKey);
    const outputHash = h.sha256(output);

    const before = Number((await getAccount(connection, providerToken)).amount);
    await program.methods
      .releaseVerified(Array.from(outputHash))
      .accounts({
        payer: settler.publicKey,
        consumer: payer.publicKey,
        job,
        vault,
        providerToken,
        feeToken,
        storageToken: null,
        authorToken: null,
        tokenProgram: TOKEN_PROGRAM_ID,
        instructions: anchor.web3.SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .preInstructions([h.ed25519IxHonest(node.publicKey, msg, sig)])
      .signers([settler])
      .rpc();

    const after = Number((await getAccount(connection, providerToken)).amount);
    assert.equal(after - before, AMOUNT - feeOf(AMOUNT));
    assert.isNull(await connection.getAccountInfo(job));
  });

  it("release_verified REJECTS a forged proof via Ed25519 index spoof (C1)", async () => {
    const node = nacl.sign.keyPair(); // escrow's provider key — never signs
    const { jobId, job, vault } = await fundJob(node.publicKey);

    const attacker = nacl.sign.keyPair(); // the key we actually sign with
    const output = Buffer.from("forged-output");
    const msg = h.resultMessage(jobId, output);
    const attackerSig = nacl.sign.detached(msg, attacker.secretKey);
    const outputHash = h.sha256(output);

    // ix0: inline the provider key but point indices at ix1 (the spoof).
    const ix0 = h.ed25519IxAt(node.publicKey, msg, attackerSig, 1);
    // ix1: honest verification of the ATTACKER key (what the precompile checks).
    const ix1 = h.ed25519IxHonest(attacker.publicKey, msg, attackerSig);

    try {
      await program.methods
        .releaseVerified(Array.from(outputHash))
        .accounts({
          payer: payer.publicKey,
          consumer: payer.publicKey,
          job,
          vault,
          providerToken,
          feeToken,
        storageToken: null,
        authorToken: null,
          tokenProgram: TOKEN_PROGRAM_ID,
          instructions: anchor.web3.SYSVAR_INSTRUCTIONS_PUBKEY,
        })
        .preInstructions([ix0, ix1])
        .rpc();
      assert.fail("forged proof was accepted — C1 not fixed");
    } catch (e: any) {
      assert.match(e.toString(), /BadSignature|6008|0x1778/);
    }
    // Funds untouched: vault still holds the full amount.
    const bal = await getAccount(connection, vault);
    assert.equal(Number(bal.amount), AMOUNT);
  });
});
