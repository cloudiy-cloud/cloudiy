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

const FEE_BPS = 400;
const feeOf = (a: number) => Math.floor((a * FEE_BPS) / 10_000);
const cut = (a: number, bps: number) => Math.floor((a * bps) / 10_000);

// Multi-payee split for the Cloudiy escrow (RFC-0004 §6).
describe("cloudiy-escrow: multi-payee split", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.CloudiyEscrow as Program<CloudiyEscrow>;
  const connection = provider.connection;
  const payer = (provider.wallet as anchor.Wallet).payer;

  let mint: PublicKey;
  let providerWallet: Keypair;
  let storagePayee: Keypair;
  let authorPayee: Keypair;
  let consumerToken: PublicKey;
  let providerToken: PublicKey;
  let feeToken: PublicKey;
  let storageToken: PublicKey;
  let authorToken: PublicKey;

  const NODE_ZERO = Array(32).fill(0);

  before(async () => {
    await h.airdrop(connection, payer.publicKey, 10);
    providerWallet = h.fundedKeypair();
    storagePayee = h.fundedKeypair();
    authorPayee = h.fundedKeypair();

    mint = await createMint(connection, payer, payer.publicKey, null, 6);
    const ata = async (owner: PublicKey) =>
      (await getOrCreateAssociatedTokenAccount(connection, payer, mint, owner, true)).address;
    consumerToken = await ata(payer.publicKey);
    providerToken = await ata(providerWallet.publicKey);
    feeToken = await ata(h.FEE_AUTHORITY);
    storageToken = await ata(storagePayee.publicKey);
    authorToken = await ata(authorPayee.publicKey);
    await mintTo(connection, payer, mint, consumerToken, payer, 5_000_000_000);
  });

  // Fund a job with an explicit split (via create_job_split).
  async function fundSplit(
    amount: number,
    storageBps: number,
    authorBps: number,
    nodePub: number[] = NODE_ZERO,
  ) {
    const jobId = h.randomJobId();
    const [job] = h.jobPda(program.programId, payer.publicKey, jobId);
    const [vault] = h.vaultPda(program.programId, job);
    await program.methods
      .createJobSplit(
        Array.from(jobId),
        new anchor.BN(amount),
        new anchor.BN(600),
        nodePub,
        storagePayee.publicKey,
        storageBps,
        authorPayee.publicKey,
        authorBps,
      )
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

  async function balances() {
    const g = async (a: PublicKey) => Number((await getAccount(connection, a)).amount);
    return {
      provider: await g(providerToken),
      fee: await g(feeToken),
      storage: await g(storageToken),
      author: await g(authorToken),
    };
  }

  it("three-way split pays provider + storage + author + fee, summing to amount", async () => {
    const amount = 1_000_000;
    const storageBps = 1000; // 10%
    const authorBps = 500; //  5%
    const { job, vault } = await fundSplit(amount, storageBps, authorBps);

    const before = await balances();
    await program.methods
      .release()
      .accounts({
        consumer: payer.publicKey,
        job,
        vault,
        providerToken,
        feeToken,
        storageToken,
        authorToken,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    const after = await balances();

    const fee = feeOf(amount);
    const storage = cut(amount, storageBps);
    const author = cut(amount, authorBps);
    const providerCut = amount - fee - storage - author;

    assert.equal(after.fee - before.fee, fee, "fee");
    assert.equal(after.storage - before.storage, storage, "storage");
    assert.equal(after.author - before.author, author, "author");
    assert.equal(after.provider - before.provider, providerCut, "provider");
    assert.equal(fee + storage + author + providerCut, amount, "sum == amount");
    // Vault + job closed.
    assert.isNull(await connection.getAccountInfo(vault));
    assert.isNull(await connection.getAccountInfo(job));
  });

  it("backwards compat: create_job (no split) pays provider = amount − fee", async () => {
    const amount = 1_000_000;
    const jobId = h.randomJobId();
    const [job] = h.jobPda(program.programId, payer.publicKey, jobId);
    const [vault] = h.vaultPda(program.programId, job);
    await program.methods
      .createJob(Array.from(jobId), new anchor.BN(amount), new anchor.BN(600), NODE_ZERO)
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

    const before = await balances();
    // No storage/author accounts needed → pass null for the optional payees.
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
    const after = await balances();

    assert.equal(after.fee - before.fee, feeOf(amount));
    assert.equal(after.provider - before.provider, amount - feeOf(amount));
    assert.equal(after.storage - before.storage, 0);
    assert.equal(after.author - before.author, 0);
  });

  it("release_verified applies the same split with a valid Ed25519 proof", async () => {
    const node = nacl.sign.keyPair();
    const amount = 2_000_000;
    const storageBps = 750;
    const authorBps = 250;
    const { jobId, job, vault } = await fundSplit(
      amount,
      storageBps,
      authorBps,
      Array.from(node.publicKey),
    );

    const output = Buffer.from("split-output");
    const msg = h.resultMessage(jobId, output);
    const sig = nacl.sign.detached(msg, node.secretKey);
    const outputHash = h.sha256(output);

    const before = await balances();
    await program.methods
      .releaseVerified(Array.from(outputHash))
      .accounts({
        payer: payer.publicKey,
        consumer: payer.publicKey,
        job,
        vault,
        providerToken,
        feeToken,
        storageToken,
        authorToken,
        tokenProgram: TOKEN_PROGRAM_ID,
        instructions: anchor.web3.SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .preInstructions([h.ed25519IxHonest(node.publicKey, msg, sig)])
      .rpc();
    const after = await balances();

    const fee = feeOf(amount);
    const storage = cut(amount, storageBps);
    const author = cut(amount, authorBps);
    assert.equal(after.fee - before.fee, fee);
    assert.equal(after.storage - before.storage, storage);
    assert.equal(after.author - before.author, author);
    assert.equal(after.provider - before.provider, amount - fee - storage - author);
  });

  it("rejects SplitTooLarge at create (fee + storage + author ≥ 10_000)", async () => {
    try {
      // 9600 + 400 fee = 10_000 → no room for the provider.
      await fundSplit(1_000_000, 9600, 0);
      assert.fail("accepted a split with no provider share");
    } catch (e: any) {
      assert.match(e.toString(), /SplitTooLarge|6011/);
    }
  });

  it("rejects MissingPayee at release when a shared payee's account is absent", async () => {
    const { job, vault } = await fundSplit(1_000_000, 1000, 0);
    try {
      await program.methods
        .release()
        .accounts({
          consumer: payer.publicKey,
          job,
          vault,
          providerToken,
          feeToken,
          storageToken: null, // storage_bps > 0 but account omitted
          authorToken: null,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();
      assert.fail("released without the required storage payee");
    } catch (e: any) {
      assert.match(e.toString(), /MissingPayee|6012/);
    }
  });

  it("rejects a wrong-owner payee token account (OwnerMismatch)", async () => {
    const { job, vault } = await fundSplit(1_000_000, 1000, 0);
    // A token account NOT owned by job.storage_payee.
    const wrong = (
      await getOrCreateAssociatedTokenAccount(connection, payer, mint, authorPayee.publicKey, true)
    ).address; // owned by authorPayee, not storagePayee
    try {
      await program.methods
        .release()
        .accounts({
          consumer: payer.publicKey,
          job,
          vault,
          providerToken,
          feeToken,
          storageToken: wrong,
          authorToken: null,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();
      assert.fail("released to a wrong-owner storage account");
    } catch (e: any) {
      assert.match(e.toString(), /OwnerMismatch|6005/);
    }
  });

  it("dust: provider absorbs the rounding remainder, nothing is lost", async () => {
    const amount = 1_000_003; // does not divide evenly by the bps
    const storageBps = 3333;
    const { job, vault } = await fundSplit(amount, storageBps, 0);

    const before = await balances();
    await program.methods
      .release()
      .accounts({
        consumer: payer.publicKey,
        job,
        vault,
        providerToken,
        feeToken,
        storageToken,
        authorToken: null,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    const after = await balances();

    const fee = feeOf(amount);
    const storage = cut(amount, storageBps);
    const providerCut = amount - fee - storage;
    assert.equal(after.fee - before.fee, fee);
    assert.equal(after.storage - before.storage, storage);
    assert.equal(after.provider - before.provider, providerCut);
    // The four shares reconstruct the exact amount — no dust lost or minted.
    assert.equal(fee + storage + 0 + providerCut, amount);
  });
});
