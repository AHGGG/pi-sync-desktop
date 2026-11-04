// Downloads configuration only, from the single reviewed public repository. Never executes fetched code.
import https from "node:https";
const MAX = 4 * 1024 * 1024;
export function fetchJson(url) {
  return new Promise((resolve, reject) => {
    const req = https.get(
      url,
      {
        headers: {
          "User-Agent": "pi-sync-desktop",
          Accept: "application/vnd.github+json",
        },
        timeout: 15000,
      },
      (res) => {
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error("PUBLIC_SOURCE_UNAVAILABLE"));
          return;
        }
        const chunks = [];
        let size = 0;
        res.on("data", (chunk) => {
          size += chunk.length;
          if (size > MAX) {
            req.destroy();
            reject(new Error("PUBLIC_SOURCE_TOO_LARGE"));
          } else chunks.push(chunk);
        });
        res.on("end", () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          } catch {
            reject(new Error("PUBLIC_SOURCE_INVALID"));
          }
        });
        res.on("error", () => reject(new Error("PUBLIC_SOURCE_UNAVAILABLE")));
      },
    );
    req.on("timeout", () => req.destroy());
    req.on("error", () => reject(new Error("PUBLIC_SOURCE_UNAVAILABLE")));
  });
}
export async function latestPublic(documents, get = fetchJson) {
  const commit = await get(
    "https://api.github.com/repos/AHGGG/my-pi-harness/commits/HEAD",
  );
  if (!/^[a-f0-9]{40}$/.test(commit?.sha))
    throw new Error("PUBLIC_SOURCE_INVALID");
  const values = {};
  for (const name of documents) {
    if (!["settings", "mcp", "subagents"].includes(name))
      throw new Error("INVALID_SELECTION");
    values[name] = await get(
      `https://raw.githubusercontent.com/AHGGG/my-pi-harness/${commit.sha}/shared/${name}.json`,
    );
  }
  return { revision: commit.sha, values };
}
