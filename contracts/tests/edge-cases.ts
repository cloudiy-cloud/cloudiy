import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, SystemProgram } from "@solana/web3.js";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import * as nacl from "tweetnacl";
import { CloudiyEscrow } from "../target/types/cloudiy_escrow";
import * as h from "./helpers";

const AMOUNT = 1_000_000;
const MAX_TIMEOUT = 30 * 24 * 60 * 60;

describe("cloudiy-escrow: edge cases", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.CloudiyEscrow as Program<CloudiyEscrow>;
  const connection = provider.connection;
  const payer = (provider.wallet as anchor.Wallet).payer;

  let mint: PublicKey;
  let providerWallet: Keypair;
  let consumerToken: PublicKey;
  let providerToken: PublicKey;
  let feeToken: PublicKey;

  before(async () => {
    await h.airdrop(connection, payer.publicKey, 10);
    providerWallet = h.fundedKeypair();
    await h.airdrop(connection, providerWallet.publicKey, 2);
    mint = await createMint(connection, payer, payer.publicKey, null, 6);
    consumerToken = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, payer.publicKey)
    ).address;
    providerToken = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, providerWallet.publicKey)
    ).address;
    feeToken = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, h.FEE_AUTHORITY)
    ).address;
    await mintTo(connection, payer, mint, consumerToken, payer, 1_000_000_000);
  });

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

  it("create_job rejects a timeout below the minimum (TimeoutTooShort)", async () => {
    try {
      await fundJob(nacl.sign.keyPair().publicKey, 30);
      assert.fail("accepted a too-short timeout");
    } catch (e: any) {
      assert.match(e.toString(), /TimeoutTooShort|6001/);
    }
  });

  it("create_job rejects a timeout above the maximum (TimeoutTooLong)", async () => {
    try {
      await fundJob(nacl.sign.keyPair().publicKey, MAX_TIMEOUT + 1);
      assert.fail("accepted a too-long timeout");
    } catch (e: any) {
      assert.match(e.toString(), /TimeoutTooLong|6002/);
    }
  });

  it("refund: provider may cancel any time, funds return to consumer", async () => {
    const { job, vault } = await fundJob(nacl.sign.keyPair().publicKey);
    const before = Number((await getAccount(connection, consumerToken)).amount);
    await program.methods
      .refund()
      .accounts({
        signer: providerWallet.publicKey,
        consumer: payer.publicKey,
        job,
        vault,
        consumerToken,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([providerWallet])
      .rpc();
    const after = Number((await getAccount(connection, consumerToken)).amount);
    assert.equal(after - before, AMOUNT);
    assert.isNull(await connection.getAccountInfo(job)); // closed
  });

  it("refund: consumer BEFORE the deadline is rejected (RefundNotAllowed)", async () => {
    const { job, vault } = await fundJob(nacl.sign.keyPair().publicKey);
    try {
      await program.methods
        .refund()
        .accounts({
          signer: payer.publicKey,
          consumer: payer.publicKey,
          job,
          vault,
          consumerToken,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();
      assert.fail("consumer refunded before the deadline");
    } catch (e: any) {
      assert.match(e.toString(), /RefundNotAllowed|6006/);
    }
  });

  it("refund: consumer AFTER the deadline succeeds", async () => {
    // Minimum on-chain timeout is 60s; fund with 60 and wait it out.
    const { job, vault } = await fundJob(nacl.sign.keyPair().publicKey, 60);
    await new Promise((r) => setTimeout(r, 63_000));
    const before = Number((await getAccount(connection, consumerToken)).amount);
    await program.methods
      .refund()
      .accounts({
        signer: payer.publicKey,
        consumer: payer.publicKey,
        job,
        vault,
        consumerToken,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    const after = Number((await getAccount(connection, consumerToken)).amount);
    assert.equal(after - before, AMOUNT);
  }).timeout(90_000);

  it("release: double-release is impossible (job closed after first)", async () => {
    const { job, vault } = await fundJob(nacl.sign.keyPair().publicKey);
    const rel = () =>
      program.methods
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
    await rel();
    try {
      await rel();
      assert.fail("second release succeeded");
    } catch (e: any) {
      // Account no longer exists / not initialized.
      assert.match(e.toString(), /AccountNotInitialized|3012|does not exist|could not find/i);
    }
  });

  it("release_verified: a signature by the WRONG key is rejected (WrongSigner)", async () => {
    const node = nacl.sign.keyPair(); // escrow key
    const { jobId, job, vault } = await fundJob(node.publicKey);
    const wrong = nacl.sign.keyPair();

    const output = Buffer.from("out");
    const input = Buffer.from("test-input");
    const msg = h.resultMessage(jobId, input, output);
    const sig = nacl.sign.detached(msg, wrong.secretKey); // signed by wrong key
    const inputHash = h.sha256(input);
    const outputHash = h.sha256(output);

    try {
      await program.methods
        .releaseVerified(Array.from(inputHash), Array.from(outputHash))
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
        // Honest instruction, but for the WRONG key — precompile passes, the
        // contract's pubkey check fails.
        .preInstructions([h.ed25519IxHonest(wrong.publicKey, msg, sig)])
        .rpc();
      assert.fail("accepted a proof by the wrong key");
    } catch (e: any) {
      assert.match(e.toString(), /WrongSigner|6009/);
    }
    assert.equal(Number((await getAccount(connection, vault)).amount), AMOUNT);
  });
});
