// The prettier formatter installs prettier into opencode's package cache (`Npm.which`, @npmcli/arborist) the first
// time a file it formats is written. Each scenario serves the registry itself (packuments and gzipped tarballs
// answered by the fake network), and the "prettier" it publishes is a shell script that rewrites the formatted file
// with what the install left in the cache: every package and its version where it landed, the bins and their
// targets, and the root package.json and lockfiles. So the recorded project file shows the installed tree, and the
// recorded requests show what was fetched.
//
// The TS reference runs arborist on its own HTTP agent, which reaches the fake network only through a process's
// proxy settings: these scenarios run against a server process. An install fetches packuments and tarballs
// concurrently, so registry requests are compared as a multiset (`unordered`).
import { createHash } from "node:crypto"
import { rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { gzipSync } from "node:zlib"
import { call, say, type Responder } from "../lib/openrouter"
import { type Ctx, type ScenarioOptions, scenario as base } from "../lib/scenario"

type Files = Record<string, string | { readonly mode: number; readonly content: string }>
interface Version {
  readonly [field: string]: unknown
  readonly files?: Files
}
interface Package {
  readonly "dist-tags": Record<string, string>
  readonly versions: Record<string, Version>
}

const REGISTRY = "https://registry.npmjs.org"

/** A ustar archive of `package/<path>` entries, gzipped (deterministic: no times, no owners). */
function tarball(files: Files) {
  const blocks: Buffer[] = []
  for (const [name, entry] of Object.entries(files)) {
    const content = Buffer.from(typeof entry === "string" ? entry : entry.content)
    const header = Buffer.alloc(512)
    header.write(`package/${name}`, 0)
    header.write(`${(typeof entry === "string" ? 0o644 : entry.mode).toString(8).padStart(7, "0")}\0`, 100)
    header.write("0000000\0", 108)
    header.write("0000000\0", 116)
    header.write(`${content.length.toString(8).padStart(11, "0")}\0`, 124)
    header.write("00000000000\0", 136)
    header.write("        ", 148)
    header.write("0", 156)
    header.write("ustar\0", 257)
    header.write("00", 263)
    let sum = 0
    for (const byte of header) sum += byte
    header.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148)
    blocks.push(header, content, Buffer.alloc((512 - (content.length % 512)) % 512))
  }
  blocks.push(Buffer.alloc(1024))
  return new Uint8Array(gzipSync(Buffer.concat(blocks)))
}

// The formatter stand-in: `prettier --write <file>` replaces the file with a report of the install.
const REPORT = `#!/bin/sh
nm=$(dirname "$0")/..
{
  echo "packages:"
  (cd "$nm" && find . -name package.json -not -path './.bin/*' | LC_ALL=C sort) | while read -r f; do
    printf '  %s %s\\n' "$(dirname "$f")" "$(sed -n 's/^  "version": "\\(.*\\)",*$/\\1/p' "$nm/$f")"
  done
  echo "files:"
  (cd "$nm" && find . -type f -not -name package.json -not -path './.bin/*' -not -name '.package-lock.json' | LC_ALL=C sort) | while read -r f; do
    x=-; [ -x "$nm/$f" ] && x=x
    printf '  %s %s %s\\n' "$x" "$f" "$(head -c 40 "$nm/$f" | tr '\\r\\n' '^|')"
  done
  echo "bins:"
  (cd "$nm" && find . -path '*/.bin/*' | LC_ALL=C sort) | while read -r b; do
    printf '  %s -> %s\\n' "$b" "$(readlink "$nm/$b")"
  done
  echo "package.json:"
  cat "$nm/../package.json"
  echo "package-lock.json:"
  grep -v '"name": ".staging-' "$nm/../package-lock.json"
  echo "node_modules/.package-lock.json:"
  grep -v '"name": ".staging-' "$nm/.package-lock.json"
} > "$2"
`

/**
 * The registry's answers for `packages` (on `host`): packuments and tarballs, never cached by the client. Each
 * scenario's packages are its own (the scenario name is in every tarball), so no install sees another's tarballs.
 */
function registry(id: string, packages: Record<string, Package>, host = REGISTRY): Responder {
  const tarballs = new Map<string, Uint8Array>()
  const packuments = new Map<string, unknown>()
  for (const [name, entry] of Object.entries(packages)) {
    const versions: Record<string, unknown> = {}
    for (const [version, { files = {}, ...manifest }] of Object.entries(entry.versions)) {
      const content = { "package.json": JSON.stringify({ name, version, ...manifest }, null, 2), "index.js": `// ${id}\n`, ...files }
      const bytes = tarball(content)
      const file = `/${name}/-/${name.split("/").pop()}-${version}.tgz`
      tarballs.set(file, bytes)
      const integrity = `sha512-${createHash("sha512").update(bytes).digest("base64")}`
      versions[version] = { name, version, ...manifest, dist: { tarball: `${host}${file}`, integrity } }
    }
    packuments.set(`/${name}`, { name, "dist-tags": entry["dist-tags"], versions })
  }
  const headers = { "cache-control": "no-store" }
  return (request) => {
    if (!request.url.startsWith(`${host}/`)) return undefined
    const pathname = decodeURIComponent(new URL(request.url).pathname)
    const bytes = tarballs.get(pathname)
    if (bytes) return { raw: bytes, headers: { ...headers, "content-type": "application/octet-stream" } }
    const packument = packuments.get(pathname)
    if (packument) return { raw: JSON.stringify(packument), headers: { ...headers, "content-type": "application/json" } }
    return { status: 404, body: { error: "Not found" }, headers }
  }
}

const prettier = (dependencies: Record<string, unknown> = {}, extra: Version = {}): Package => ({
  "dist-tags": { latest: "3.0.0" },
  versions: {
    "3.0.0": { bin: { prettier: "bin/prettier.sh" }, ...dependencies, ...extra, files: { "bin/prettier.sh": { mode: 0o755, content: REPORT }, ...extra.files } },
  },
})

const write = (text = "const answer = 42\n") => [call("write", { path: "src/answer.js", content: text }), say("Written.")]

const scenario = (name: string, options: ScenarioOptions, body?: (ctx: Ctx) => Promise<void>) =>
  base(
    name,
    {
      // `formatter: true` turns the built-in formatters on; prettier applies where package.json depends on it.
      config: { formatter: true },
      files: { "package.json": JSON.stringify({ devDependencies: { prettier: "^3.0.0" } }, null, 2) },
      processOnly: "arborist reaches the registry over its own HTTP agent, which only a process's proxy settings send to the fake network",
      unordered: (request) => request.url.startsWith(`${REGISTRY}/`) || request.url.startsWith("https://npm.example.test/"),
      timeout: 120_000,
      ...options,
    },
    async (ctx) => {
      // The package cache and the npm cache are the sandbox's, shared by the scenarios of this file.
      rmSync(path.join(process.env.XDG_CACHE_HOME!, "opencode", "npm"), { recursive: true, force: true })
      rmSync(path.join(process.env.HOME!, ".npm"), { recursive: true, force: true })
      try {
        await body?.(ctx)
        const id = await ctx.session()
        await ctx.prompt(id, "Write src/answer.js")
      } finally {
        rmSync(path.join(process.env.HOME!, ".npmrc"), { force: true })
      }
    },
  )

scenario("prettier and its dependencies install into the package cache, hoisted", {
  turns: write(),
  responders: [
    registry("hoisted", {
      prettier: prettier({ dependencies: { "left-pad": "^1.1.0", chalk: "~2.4.0" } }),
      "left-pad": { "dist-tags": { latest: "1.3.0" }, versions: { "1.1.0": {}, "1.3.0": { license: "WTFPL" } } },
      chalk: { "dist-tags": { latest: "5.0.0" }, versions: { "2.4.1": { dependencies: { "left-pad": "1" } }, "2.4.2": { dependencies: { "left-pad": "1.x" }, license: { type: "MIT" } }, "5.0.0": {} } },
    }),
  ],
})

scenario("conflicting versions of a dependency nest under their dependent", {
  turns: write(),
  responders: [
    registry("nested", {
      prettier: prettier({ dependencies: { a: "^1.0.0", b: "^1.0.0" } }),
      a: { "dist-tags": { latest: "1.1.0" }, versions: { "1.0.0": { dependencies: { c: "^1.0.0" } }, "1.1.0": { dependencies: { c: "^1.0.0" } } } },
      b: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { dependencies: { c: "^2.0.0" }, bin: "cli.js", files: { "cli.js": "#!/bin/sh\r\necho b\r\n" } } } },
      c: { "dist-tags": { latest: "2.1.0" }, versions: { "1.0.0": {}, "1.5.0": {}, "2.0.0": {}, "2.1.0": { engines: { node: ">=8" } } } },
    }),
  ],
})

scenario("a version is picked as npm-pick-manifest picks it: the latest tag, then not deprecated, engines, newest", {
  turns: write(),
  responders: [
    registry("picked", {
      prettier: prettier({ dependencies: { tagged: "^1.0.0", ranked: "^1.2.0", hyphen: "1.2.3 - 1.4", either: ">=1.0.0 <1.2.0 || ^2.1", pre: "^2.0.0-beta.1", loose: "~1", exact: "=v1.0.0", star: "*" } }),
      tagged: { "dist-tags": { latest: "1.1.0" }, versions: { "1.0.0": {}, "1.1.0": {}, "1.9.0": {} } },
      ranked: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": {}, "1.2.0": { engines: { node: ">=99" } }, "1.3.0": { deprecated: "use 2" }, "1.1.0": {} } },
      hyphen: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": {}, "1.2.3": {}, "1.4.9": {}, "1.5.0": {} } },
      either: { "dist-tags": { latest: "3.0.0" }, versions: { "1.1.9": {}, "1.2.0": {}, "2.0.5": {}, "2.4.0": {}, "3.0.0": {} } },
      pre: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": {}, "2.0.0-beta.2": {}, "2.0.1-beta.1": {}, "2.0.0": {}, "3.0.0-rc.1": {} } },
      loose: { "dist-tags": { latest: "2.0.0" }, versions: { "1.0.0": {}, "1.8.0": {}, "2.0.0": {} } },
      exact: { "dist-tags": { latest: "1.1.0" }, versions: { "1.0.0": {}, "1.1.0": {} } },
      star: { "dist-tags": { latest: "2.0.0-next.1" }, versions: { "1.0.0": {}, "2.0.0-next.1": {} } },
    }),
  ],
})

scenario("optional dependencies for another platform are left out, peers are installed beside their dependent", {
  turns: write(),
  responders: [
    registry("platform", {
      prettier: prettier({
        optionalDependencies: { "native-darwin": "1.0.0", "native-linux": "1.0.0", missing: "^1.0.0" },
        dependencies: { plugin: "^1.0.0" },
      }),
      "native-darwin": { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { os: ["darwin"], cpu: ["arm64"] } } },
      "native-linux": { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { os: ["linux"], libc: ["glibc"] } } },
      plugin: {
        "dist-tags": { latest: "1.2.0" },
        versions: { "1.2.0": { peerDependencies: { host: "^2.0.0", extra: "*" }, peerDependenciesMeta: { extra: { optional: true } } } },
      },
      host: { "dist-tags": { latest: "3.0.0" }, versions: { "2.0.0": {}, "2.2.0": {}, "3.0.0": {} } },
      extra: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": {} } },
    }),
  ],
})

scenario("an install that fails leaves the file as written", {
  turns: write("const answer =   42\n"),
  responders: [
    registry("failed", {
      prettier: prettier({ dependencies: { gone: "^1.0.0", broken: "^1.0.0" } }),
      broken: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { os: ["!linux"] } } },
    }),
  ],
})

scenario(
  "the user's npm config: credentials, a scoped registry and the install strategy",
  {
    turns: write(),
    responders: [
      registry("config", {
        prettier: prettier({ dependencies: { a: "^1.0.0", "@corp/b": "^1.0.0" } }),
        a: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { dependencies: { c: "^1.0.0" } } } },
        c: { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": {} } },
      }),
      registry("config", { "@corp/b": { "dist-tags": { latest: "1.0.0" }, versions: { "1.0.0": { dependencies: { c: "^1.0.0" } } } } }, "https://npm.example.test"),
    ],
  },
  async () => {
    writeFileSync(
      path.join(process.env.HOME!, ".npmrc"),
      [
        "; the public registry, with a token",
        "//registry.npmjs.org/:_authToken=${CONFORMANCE_MISSING_TOKEN?}tok-123",
        "@corp:registry=https://npm.example.test/",
        "//npm.example.test/:username=deploy",
        `//npm.example.test/:_password=${Buffer.from("s3cret").toString("base64")}`,
        "install-strategy=nested",
        "",
      ].join("\n"),
    )
  },
)
