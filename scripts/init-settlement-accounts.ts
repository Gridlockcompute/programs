/**
 * Create customer + worker stake LOCK token accounts and fund them for settlement.
 *
 * Usage:
 *   npx tsx scripts/init-settlement-accounts.ts
 *   npx tsx scripts/init-settlement-accounts.ts <worker_solana_pubkey>
 */
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createAccount,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountIdempotentInstruction,
} from "@solana/spl-token";
import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";
import { HELIUS_DEVNET } from "./program-ids.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FUND_AMOUNT = 50_000_000_000n; // 50 LOCK

async function fund(conn: Connection, payer: Keypair, mint: PublicKey, account: PublicKey) {
  const tx = new Transaction().add(
    createMintToInstruction(mint, account, payer.publicKey, FUND_AMOUNT, [], TOKEN_2022_PROGRAM_ID),
  );
  await sendAndConfirmTransaction(conn, tx, [payer]);
}

async function main() {
  const mintPath = path.join(__dirname, "..", "lock-mint.json");
  if (!fs.existsSync(mintPath)) {
    console.error("Run create-lock-mint.ts first");
    process.exit(1);
  }
  const cfg = JSON.parse(fs.readFileSync(mintPath, "utf-8")) as { mint: string };
  const mint = new PublicKey(cfg.mint);

  const workerPubkeyArg = process.argv[2];
  const payerPath = path.join(process.env.HOME ?? "/root", ".config/solana/id.json");
  const payer = Keypair.fromSecretKey(Buffer.from(JSON.parse(fs.readFileSync(payerPath, "utf8"))));
  const conn = new Connection(HELIUS_DEVNET, "confirmed");

  const customerAta = getAssociatedTokenAddressSync(
    mint,
    payer.publicKey,
    false,
    TOKEN_2022_PROGRAM_ID,
  );

  const setupTx = new Transaction().add(
    createAssociatedTokenAccountIdempotentInstruction(
      payer.publicKey,
      customerAta,
      payer.publicKey,
      mint,
      TOKEN_2022_PROGRAM_ID,
    ),
  );

  let workerStake: PublicKey;
  if (workerPubkeyArg) {
    const workerPk = new PublicKey(workerPubkeyArg);
    workerStake = getAssociatedTokenAddressSync(mint, workerPk, false, TOKEN_2022_PROGRAM_ID);
    setupTx.add(
      createAssociatedTokenAccountIdempotentInstruction(
        payer.publicKey,
        workerStake,
        workerPk,
        mint,
        TOKEN_2022_PROGRAM_ID,
      ),
    );
  } else {
    workerStake = await createAccount(
      conn,
      payer,
      mint,
      payer.publicKey,
      Keypair.generate(),
      { commitment: "confirmed" },
      TOKEN_2022_PROGRAM_ID,
    );
  }

  await sendAndConfirmTransaction(conn, setupTx, [payer]);
  await fund(conn, payer, mint, customerAta);
  await fund(conn, payer, mint, workerStake);

  console.log(`customerWallet: ${customerAta.toBase58()} (50 LOCK)`);
  console.log(`defaultWorkerStake: ${workerStake.toBase58()} (50 LOCK)`);

  const updated = {
    ...cfg,
    customerWallet: customerAta.toBase58(),
    defaultWorkerStake: workerStake.toBase58(),
    settlementFundedAt: new Date().toISOString(),
  };
  fs.writeFileSync(mintPath, JSON.stringify(updated, null, 2));

  console.log("\nAdd to gridlock-backend/.env:");
  console.log(`CUSTOMER_WALLET=${customerAta.toBase58()}`);
  console.log(`DEFAULT_WORKER_STAKE=${workerStake.toBase58()}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
