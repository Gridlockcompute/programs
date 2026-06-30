/**
 * Create Token-2022 vault accounts owned by the FeeCollector PDA and seed fee_vault.
 *
 * Usage: npx tsx scripts/init-fee-vaults.ts
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
} from "@solana/spl-token";
import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";
import { HELIUS_DEVNET, PROGRAM_IDS } from "./program-ids.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const FEE_COLLECTOR_PROGRAM = new PublicKey(PROGRAM_IDS.feeCollector);
const SEED_AMOUNT = 100_000_000_000n; // 100 LOCK (9 decimals) for devnet testing

async function createVault(
  conn: Connection,
  payer: Keypair,
  mint: PublicKey,
  owner: PublicKey,
  label: string,
): Promise<PublicKey> {
  const account = Keypair.generate();
  const pubkey = await createAccount(
    conn,
    payer,
    mint,
    owner,
    account,
    { commitment: "confirmed" },
    TOKEN_2022_PROGRAM_ID,
  );
  console.log(`  ${label}: ${pubkey.toBase58()}`);
  return pubkey;
}

async function main() {
  const mintPath = path.join(__dirname, "..", "lock-mint.json");
  if (!fs.existsSync(mintPath)) {
    console.error("Run scripts/create-lock-mint.ts first");
    process.exit(1);
  }
  const mintConfig = JSON.parse(fs.readFileSync(mintPath, "utf-8")) as {
    mint: string;
    mintAuthority: string;
    feeCollectorPda: string;
  };

  const conn = new Connection(HELIUS_DEVNET, "confirmed");
  const payerPath = path.join(process.env.HOME ?? "/root", ".config/solana/id.json");
  const payer = Keypair.fromSecretKey(
    Buffer.from(JSON.parse(fs.readFileSync(payerPath, "utf-8"))),
  );

  const mint = new PublicKey(mintConfig.mint);
  const feeCollectorPda = new PublicKey(mintConfig.feeCollectorPda);

  console.log(`Payer: ${payer.publicKey.toBase58()}`);
  console.log(`LOCK mint: ${mint.toBase58()}`);
  console.log(`Vault owner (FeeCollector PDA): ${feeCollectorPda.toBase58()}`);
  console.log("\nCreating vault token accounts...");

  const feeVault = await createVault(conn, payer, mint, feeCollectorPda, "fee_vault");
  const stakerPool = await createVault(conn, payer, mint, feeCollectorPda, "staker_pool");
  const workerPayout = await createVault(conn, payer, mint, feeCollectorPda, "worker_payout");
  const treasury = await createVault(conn, payer, mint, feeCollectorPda, "treasury");
  const burnVault = await createVault(conn, payer, mint, feeCollectorPda, "burn_vault");

  console.log(`\nMinting ${SEED_AMOUNT} base units to fee_vault...`);
  const mintTx = new Transaction().add(
    createMintToInstruction(
      mint,
      feeVault,
      payer.publicKey,
      SEED_AMOUNT,
      [],
      TOKEN_2022_PROGRAM_ID,
    ),
  );
  const mintSig = await sendAndConfirmTransaction(conn, mintTx, [payer]);
  console.log(`  mint tx: ${mintSig}`);

  const output = {
    ...mintConfig,
    vaults: {
      feeVault,
      stakerPool,
      workerPayout,
      treasury,
      burnVault,
    },
    vaultOwner: feeCollectorPda.toBase58(),
    seededFeeVaultAmount: SEED_AMOUNT.toString(),
    updatedAt: new Date().toISOString(),
  };

  fs.writeFileSync(mintPath, JSON.stringify(output, null, 2));
  console.log(`\nUpdated ${mintPath}`);
  console.log("\nAdd to gridlock-backend/.env:");
  console.log(`LOCK_MINT=${mint.toBase58()}`);
  console.log(`FEE_VAULT=${feeVault.toBase58()}`);
  console.log(`STAKER_POOL=${stakerPool.toBase58()}`);
  console.log(`WORKER_PAYOUT=${workerPayout.toBase58()}`);
  console.log(`TREASURY=${treasury.toBase58()}`);
  console.log(`BURN_VAULT=${burnVault.toBase58()}`);
  console.log("SOLANA_SETTLEMENT_ENABLED=true");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
