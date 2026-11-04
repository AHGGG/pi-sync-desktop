// Scan only version-controlled candidate product files, never runtime profiles.
import fs from 'node:fs/promises';
import path from 'node:path';
const roots=['backend','crates','docs','public-harness','scripts','.github'];
const forbidden=[/-----BEGIN (?:OPENSSH|RSA|EC|DSA|PRIVATE).*?KEY-----/,/\bgh[pousr]_[A-Za-z0-9]{25,}\b/,/\bgithub_pat_[A-Za-z0-9_]{30,}\b/,/\bsk-(?:proj-)?[A-Za-z0-9_-]{24,}\b/,/\bAKIA[A-Z0-9]{16}\b/];
async function scan(dir){for(const entry of await fs.readdir(dir,{withFileTypes:true})){const file=path.join(dir,entry.name);if(entry.isDirectory())await scan(file);else {if(/(?:^|\/)(?:models|auth|profiles|known_hosts|last-selection)\.json$/.test(file))throw Error(`Private configuration filename: ${file}`);const text=await fs.readFile(file,'utf8');if(forbidden.some(pattern=>pattern.test(text)))throw Error(`Possible credential material: ${file}`);}}}
for(const root of roots)await scan(root);
console.log('Public-source credential-pattern scan passed. Review remains required.');
