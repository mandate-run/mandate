// Decodes an exact Hedera payment payload without a network: transaction id,
// node account ids, validity and transfers. Input is a PAYMENT-SIGNATURE
// header or a bare base64 transaction.
//   pnpm exec tsx src/decode.ts <base64>
import { Transaction, TransferTransaction } from "@x402/hedera";

const arg = process.argv[2];
if (arg === undefined) {
  console.error("usage: pnpm exec tsx src/decode.ts <payment-signature or transaction base64>");
  process.exit(2);
}
let transaction = arg;
try {
  const payload = JSON.parse(Buffer.from(arg, "base64").toString("utf8")) as { payload?: { transaction?: string } };
  if (typeof payload.payload?.transaction === "string") transaction = payload.payload.transaction;
} catch {
  // bare transaction bytes
}
const decoded = Transaction.fromBytes(Buffer.from(transaction, "base64"));
if (!(decoded instanceof TransferTransaction)) {
  console.error(`not a TransferTransaction: ${decoded.constructor.name}`);
  process.exit(1);
}
const tx: TransferTransaction = decoded;
const hbar: Record<string, string> = {};
for (const [account, amount] of tx.hbarTransfers) hbar[account.toString()] = amount.toTinybars().toString();
const tokens: Record<string, Record<string, string>> = {};
for (const [token, transfers] of tx.tokenTransfers) {
  tokens[token.toString()] = Object.fromEntries([...transfers].map(([account, amount]) => [account.toString(), amount.toString()]));
}
console.log(
  JSON.stringify(
    {
      transactionId: tx.transactionId?.toString() ?? null,
      payerAccount: tx.transactionId?.accountId?.toString() ?? null,
      validStart: tx.transactionId?.validStart?.toDate().toISOString() ?? null,
      validDurationSeconds: tx.transactionValidDuration,
      nodeAccountIds: (tx.nodeAccountIds ?? []).map((id) => id.toString()),
      hbarTransfers: hbar,
      tokenTransfers: tokens,
    },
    null,
    2,
  ),
);
