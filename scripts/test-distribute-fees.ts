/** Smoke-test distribute_fees with 0.02 LOCK (20_000_000 base units). */
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import {
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { Connection } from "@solana/web3.js";
import { HELIUS_DEVNET, PROGRAM_IDS } from "./program-ids.js";
import lockMint from "../lock-mint.json" with { type: "json" };

const TOKEN_2022 = new PublicKey("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

function anchorDiscriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

function borshU64(value: number): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(BigInt(value));
  return buf;
}

async function main() {
  const kpPath = join(homedir(), ".config/solana/id.json");
  const kp = Keypair.fromSecretKey(Buffer.from(JSON.parse(readFileSync(kpPath, "utf8"))));
  const conn = new Connection(HELIUS_DEVNET, "confirmed");

  const [collector] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_collector")],
    new PublicKey(PROGRAM_IDS.feeCollector),
  );

  const amount = 20_000_000; // 0.02 LOCK
  const data = Buffer.concat([anchorDiscriminator("distribute_fees"), borshU64(amount)]);

  const keys = [
    { pubkey: collector, isSigner: false, isWritable: false },
    { pubkey: new PublicKey(lockMint.vaults.feeVault), isSigner: false, isWritable: true },
    { pubkey: new PublicKey(lockMint.vaults.stakerPool), isSigner: false, isWritable: true },
    { pubkey: new PublicKey(lockMint.vaults.workerPayout), isSigner: false, isWritable: true },
    { pubkey: new PublicKey(lockMint.vaults.treasury), isSigner: false, isWritable: true },
    { pubkey: new PublicKey(lockMint.vaults.burnVault), isSigner: false, isWritable: true },
    { pubkey: new PublicKey(lockMint.mint), isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022, isSigner: false, isWritable: false },
  ];

  const ix = new TransactionInstruction({
    programId: new PublicKey(PROGRAM_IDS.feeCollector),
    keys,
    data,
  });

  const { blockhash } = await conn.getLatestBlockhash();
  const msg = new TransactionMessage({
    payerKey: kp.publicKey,
    recentBlockhash: blockhash,
    instructions: [ix],
  }).compileToV0Message();
  const tx = new VersionedTransaction(msg);
  tx.sign([kp]);

  const sig = await conn.sendTransaction(tx);
  await conn.confirmTransaction(sig, "confirmed");
  console.log(`distribute_fees ok: https://explorer.solana.com/tx/${sig}?cluster=devnet`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
