import * as anchor from "@coral-xyz/anchor";
import { PublicKey, TransactionInstruction, Connection, Keypair } from "@solana/web3.js";
import { createHash } from "crypto";

export const ED25519_PROGRAM_ID = new PublicKey(
  "Ed25519SigVerify111111111111111111111111111"
);
export const RESULT_DOMAIN = Buffer.from("cloudiy/result/v1");
export const FEE_AUTHORITY = new PublicKey(
  "GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP"
);

/** Format 16 raw bytes as a lowercase 8-4-4-4-12 UUID string (matches the on-chain `uuid_string`). */
export function uuidString(bytes: Uint8Array): string {
  const hex = Buffer.from(bytes).toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(
    16,
    20
  )}-${hex.slice(20, 32)}`;
}

/** Reconstruct the exact message a provider node signs: DOMAIN \0 uuid \0 sha256(output). */
export function resultMessage(jobId: Uint8Array, output: Buffer): Buffer {
  const outputHash = createHash("sha256").update(output).digest();
  return Buffer.concat([
    RESULT_DOMAIN,
    Buffer.from([0]),
    Buffer.from(uuidString(jobId), "ascii"),
    Buffer.from([0]),
    outputHash,
  ]);
}

export function sha256(data: Buffer): Buffer {
  return createHash("sha256").update(data).digest();
}

/**
 * Build an Ed25519 precompile instruction whose three offset `instruction_index`
 * fields are set to `ixIndex`. `0xFFFF` = "this instruction" (the only form the
 * escrow accepts). Any other value is the precompile-spoofing shape used to
 * prove the contract rejects it. Mirrors the Rust `ed25519_verify_ix_at`.
 */
export function ed25519IxAt(
  pubkey: Uint8Array,
  message: Buffer,
  signature: Uint8Array,
  ixIndex: number
): TransactionInstruction {
  const pkOff = 2 + 14; // 16
  const sigOff = pkOff + 32; // 48
  const msgOff = sigOff + 64; // 112

  const header = Buffer.alloc(16);
  header.writeUInt8(1, 0); // one signature
  header.writeUInt8(0, 1); // padding
  header.writeUInt16LE(sigOff, 2);
  header.writeUInt16LE(ixIndex, 4);
  header.writeUInt16LE(pkOff, 6);
  header.writeUInt16LE(ixIndex, 8);
  header.writeUInt16LE(msgOff, 10);
  header.writeUInt16LE(message.length, 12);
  header.writeUInt16LE(ixIndex, 14);

  const data = Buffer.concat([
    header,
    Buffer.from(pubkey),
    Buffer.from(signature),
    message,
  ]);
  return new TransactionInstruction({
    keys: [],
    programId: ED25519_PROGRAM_ID,
    data,
  });
}

/** The honest Ed25519 verify instruction (all indices = 0xFFFF). */
export function ed25519IxHonest(
  pubkey: Uint8Array,
  message: Buffer,
  signature: Uint8Array
): TransactionInstruction {
  return ed25519IxAt(pubkey, message, signature, 0xffff);
}

/** PDA for a job: ["job", consumer, jobId]. */
export function jobPda(
  programId: PublicKey,
  consumer: PublicKey,
  jobId: Uint8Array
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("job"), consumer.toBuffer(), Buffer.from(jobId)],
    programId
  );
}

/** PDA for a vault: ["vault", job]. */
export function vaultPda(programId: PublicKey, job: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("vault"), job.toBuffer()],
    programId
  );
}

export async function airdrop(connection: Connection, to: PublicKey, sol = 5) {
  const sig = await connection.requestAirdrop(to, sol * anchor.web3.LAMPORTS_PER_SOL);
  const bh = await connection.getLatestBlockhash();
  await connection.confirmTransaction({ signature: sig, ...bh }, "confirmed");
}

export function randomJobId(): Uint8Array {
  return Uint8Array.from(anchor.web3.Keypair.generate().secretKey.slice(0, 16));
}

export function fundedKeypair(): Keypair {
  return anchor.web3.Keypair.generate();
}
