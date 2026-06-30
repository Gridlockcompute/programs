/**
 * Create the LOCK Token-2022 mint with extensions for Gridlock devnet.
 *
 * Usage: npx tsx scripts/create-lock-mint.ts
 */

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  TOKEN_2022_PROGRAM_ID,
  createInitializeMintInstruction,
  createInitializePermanentDelegateInstruction,
  createInitializeInterestBearingMintInstruction,
  ExtensionType,
  getMintLen,
} from "@solana/spl-token";
import * as fs from "fs";
import * as path from "path";
import { fileURLToPath } from "url";
import { HELIUS_DEVNET, PROGRAM_IDS } from "./program-ids.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const SLA_ENFORCER_PROGRAM = new PublicKey(PROGRAM_IDS.slaEnforcer);
const FEE_COLLECTOR_PROGRAM = new PublicKey(PROGRAM_IDS.feeCollector);

const LOCK_DECIMALS = 9;

async function main() {
  const conn = new Connection(HELIUS_DEVNET, "confirmed");

  const payerPath = path.join(process.env.HOME ?? "/root", ".config/solana/id.json");
  if (!fs.existsSync(payerPath)) {
    console.error(`Keypair not found at ${payerPath}`);
    process.exit(1);
  }
  const payer = Keypair.fromSecretKey(
    Buffer.from(JSON.parse(fs.readFileSync(payerPath, "utf-8"))),
  );
  console.log(`Payer: ${payer.publicKey.toBase58()}`);
  console.log(`Balance: ${(await conn.getBalance(payer.publicKey)) / 1e9} SOL`);

  const [slaEnforcerPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("sla_enforcer")],
    SLA_ENFORCER_PROGRAM,
  );
  const [feeCollectorPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("fee_collector")],
    FEE_COLLECTOR_PROGRAM,
  );

  console.log(`SLAEnforcer PDA (PermanentDelegate): ${slaEnforcerPda.toBase58()}`);
  console.log(`FeeCollector PDA (vault owner): ${feeCollectorPda.toBase58()}`);

  const mintKeypair = Keypair.generate();
  const mintAddress = mintKeypair.publicKey;
  console.log(`\nLOCK Mint: ${mintAddress.toBase58()}`);

  // TransferHook omitted until fee_collector implements SPL hook execute ix.
  const extensions = [
    ExtensionType.PermanentDelegate,
    ExtensionType.InterestBearingConfig,
  ];
  const mintLen = getMintLen(extensions);
  const lamports = await conn.getMinimumBalanceForRentExemption(mintLen);

  const tx = new Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: mintAddress,
      space: mintLen,
      lamports,
      programId: TOKEN_2022_PROGRAM_ID,
    }),
    createInitializePermanentDelegateInstruction(
      mintAddress,
      slaEnforcerPda,
      TOKEN_2022_PROGRAM_ID,
    ),
    createInitializeInterestBearingMintInstruction(
      mintAddress,
      payer.publicKey,
      800,
      TOKEN_2022_PROGRAM_ID,
    ),
    createInitializeMintInstruction(
      mintAddress,
      LOCK_DECIMALS,
      payer.publicKey,
      null,
      TOKEN_2022_PROGRAM_ID,
    ),
  );

  console.log("\nCreating LOCK mint...");
  const sig = await sendAndConfirmTransaction(conn, tx, [payer, mintKeypair]);
  console.log(`Tx: https://explorer.solana.com/tx/${sig}?cluster=devnet`);

  const output = {
    mint: mintAddress.toBase58(),
    mintAuthority: payer.publicKey.toBase58(),
    permanentDelegate: slaEnforcerPda.toBase58(),
    feeCollectorPda: feeCollectorPda.toBase58(),
    transferHookProgram: null,
    decimals: LOCK_DECIMALS,
    cluster: "devnet",
    createdAt: new Date().toISOString(),
    txSig: sig,
  };

  const outPath = path.join(__dirname, "..", "lock-mint.json");
  fs.writeFileSync(outPath, JSON.stringify(output, null, 2));
  console.log(`\nSaved ${outPath}`);
  console.log("\nNext: npx tsx scripts/init-fee-vaults.ts");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
