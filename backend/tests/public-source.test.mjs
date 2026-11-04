import test from "node:test";
import assert from "node:assert/strict";
import { latestPublic } from "../public-source.mjs";
test("latest public config files are fetched at one pinned commit, never executable files", async () => {
  const urls = [];
  const revision = "a".repeat(40);
  const result = await latestPublic(["settings", "mcp"], async (url) => {
    urls.push(url);
    return url.endsWith("/HEAD") ? { sha: revision } : {};
  });
  assert.equal(result.revision, revision);
  assert.deepEqual(urls, [
    "https://api.github.com/repos/AHGGG/my-pi-harness/commits/HEAD",
    `https://raw.githubusercontent.com/AHGGG/my-pi-harness/${revision}/shared/settings.json`,
    `https://raw.githubusercontent.com/AHGGG/my-pi-harness/${revision}/shared/mcp.json`,
  ]);
});
test("network failure never silently falls back to bundled config", async () => {
  await assert.rejects(
    latestPublic(["settings"], async () => {
      throw Error("PUBLIC_SOURCE_UNAVAILABLE");
    }),
    /PUBLIC_SOURCE_UNAVAILABLE/,
  );
});
