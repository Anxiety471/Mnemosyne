// @vitest-environment node
// Hashing runs against Node's own crypto: the worker path is what ships, but the digest is
// the contract, and Node is the independent implementation to check it against.
import { createHash } from "node:crypto";
import { expect, test } from "vitest";
import { hashFile } from "../src/checksum";

for (const size of [0, 3, 4 * 1024 * 1024 + 17]) {
  test(`SHA-256 agrees with Node for ${size} bytes`, async () => {
    const bytes = Buffer.alloc(size, 0x61);
    expect(await hashFile(new Blob([bytes]))).toBe(createHash("sha256").update(bytes).digest("hex"));
  });
}
