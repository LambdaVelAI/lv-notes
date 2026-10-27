// Node IDs in lv-core's text form: `node_` plus a hyphenated UUIDv7.
// The editor assigns them, so a node keeps one identity from the moment it
// is typed, and every op the core derives addresses it by that ID.

const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

let lastMs = 0;
let counter = 0;

/** A UUIDv7: 48-bit millisecond time, then random bits (RFC 9562). */
export function uuidv7(now = Date.now()): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  // Keep IDs made in the same millisecond in order, as Rust's do.
  if (now <= lastMs) {
    now = lastMs;
    counter = (counter + 1) & 0xfff;
  } else {
    lastMs = now;
    // A random start with headroom for 2048 more IDs this millisecond.
    counter = ((bytes[6] << 8) | bytes[7]) & 0x7ff;
  }
  for (let i = 5; i >= 0; i--) {
    bytes[i] = now & 0xff;
    now = Math.floor(now / 256);
  }
  bytes[6] = 0x70 | ((counter >> 8) & 0x0f);
  bytes[7] = counter & 0xff;
  bytes[8] = 0x80 | (bytes[8] & 0x3f);
  const h = hex(bytes);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

export const newNodeId = () => `node_${uuidv7()}`;

const NODE_ID = /^node_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const isNodeId = (s: unknown): s is string => typeof s === "string" && NODE_ID.test(s);
