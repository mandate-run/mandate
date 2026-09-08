// One line per request for Proctor: route, payment id, hash of the signed
// payment, whether settlement was called, and the hash of what was served.
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

export type Source = "live" | "store" | "conflict" | "unpaid" | "dropped" | "rejected";

export interface JournalEntry {
  ts: string;
  route: string;
  payment_id: string | null;
  signed_payload_hash: string | null;
  settle_called: boolean;
  /** Settlement was called and the facilitator reported success. */
  settle_ok: boolean;
  served_hash: string | null;
  status: number;
  source: Source;
}

export function sha256Hex(data: string | Uint8Array): string {
  return createHash("sha256").update(data).digest("hex");
}

/** Append-only JSON lines. A null path keeps entries in memory only. */
export class Journal {
  private readonly entries: JournalEntry[] = [];

  constructor(private readonly path: string | null) {}

  write(entry: JournalEntry): void {
    this.entries.push(entry);
    if (this.path === null) return;
    mkdirSync(dirname(this.path), { recursive: true });
    appendFileSync(this.path, `${JSON.stringify(entry)}\n`);
  }

  all(): readonly JournalEntry[] {
    return this.entries;
  }
}
