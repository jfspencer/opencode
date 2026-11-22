// Recorded expected outputs. Goldens are always produced by the TS reference, never written by hand.
//   UPDATE_GOLDENS=1   re-record every golden from the current run
//   CONFORMANCE_STRICT=1  fail when a golden is missing instead of recording it
//   CONFORMANCE_ACTUAL=<dir>  also write each mismatching result there (same layout as goldens/, blobs expanded),
//                             for `bun diff.ts <dir>`
//
// Large values that repeat across goldens (the system prompt, the tool list) are stored once, content-addressed,
// under goldens/_blobs/ and referenced by hash. Comparison always expands them, so equality is exact.
import { expect } from "bun:test"
import { createHash } from "node:crypto"
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs"
import path from "node:path"
import { DATE, PID_KEY } from "./normalize"

const ROOT = path.join(import.meta.dir, "..", "goldens")
export const BLOBS = path.join(ROOT, "_blobs")
const BLOB_MIN = 4096
const BLOB_REF = "<blob>"
// The sandbox lives under the platform temp directory, which is `/tmp` on the Linux that recorded the goldens and
// `/private/var/folders/.../T` on macOS. Only the parent path differs: root and project names are normalized.
const TMP_PARENT = process.env.CONFORMANCE_ROOT ? path.dirname(process.env.CONFORMANCE_ROOT) : "/tmp"
const TMP_PARENT_ENCODED = TMP_PARENT ? encodeURIComponent(TMP_PARENT) : ""

export function slug(text: string) {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
}

export function goldenPath(file: string, name: string) {
  return path.join(ROOT, slug(path.basename(file).replace(/\.test\.tsx?$/, "")), `${slug(name)}.json`)
}

export function matchGolden(file: string, name: string, actual: unknown) {
  const target = goldenPath(file, name)
  if (process.env.UPDATE_GOLDENS || (!existsSync(target) && !process.env.CONFORMANCE_STRICT)) {
    mkdirSync(path.dirname(target), { recursive: true })
    writeFileSync(target, JSON.stringify(compact(actual), null, 2) + "\n")
    return
  }
  if (!existsSync(target)) throw new Error(`missing golden ${path.relative(process.cwd(), target)}`)
  const expected = lenient(expand(JSON.parse(readFileSync(target, "utf8"))))
  const received = lenient(JSON.parse(JSON.stringify(actual)))
  const out = process.env.CONFORMANCE_ACTUAL
  if (out && !Bun.deepEquals(received, expected)) {
    const file = path.join(out, path.relative(ROOT, target))
    mkdirSync(path.dirname(file), { recursive: true })
    writeFileSync(file, JSON.stringify(received, null, 2) + "\n")
  }
  expect(received).toEqual(expected)
}

/** Replace large strings and tool lists with references to content-addressed blobs. */
export function compact(value: unknown, key = ""): unknown {
  if (typeof value === "string") return value.length > BLOB_MIN ? { [BLOB_REF]: store(value) } : value
  if (Array.isArray(value)) {
    const items = value.map((item) => compact(item))
    return key === "tools" && JSON.stringify(items).length > BLOB_MIN ? { [BLOB_REF]: store(items) } : items
  }
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, compact(v, k)]))
  return value
}

/** Inverse of compact. */
export function expand(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(expand)
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>
    const keys = Object.keys(record)
    if (keys.length === 1 && keys[0] === BLOB_REF && typeof record[BLOB_REF] === "string")
      return expand(JSON.parse(readFileSync(blobPath(record[BLOB_REF] as string), "utf8")))
    return Object.fromEntries(keys.map((k) => [k, expand(record[k])]))
  }
  return value
}

export const blobPath = (hash: string) => path.join(BLOBS, hash.slice(0, 2), `${hash}.json`)

function store(value: unknown) {
  const text = JSON.stringify(value)
  const hash = createHash("sha256").update(text).digest("hex")
  const target = blobPath(hash)
  if (!existsSync(target)) {
    mkdirSync(path.dirname(target), { recursive: true })
    // Atomic: parallel shards may store the same blob at once.
    const temp = `${target}.${process.pid}.${Math.random().toString(36).slice(2)}.tmp`
    writeFileSync(temp, text)
    renameSync(temp, target)
  }
  return hash
}

// Equality rules added after goldens were recorded apply to both sides at compare time.
function lenient(value: unknown, key = ""): unknown {
  if (typeof value === "string")
    return value
      .replace(DATE, "<date>")
      // Goldens are recorded on Linux; these facts legitimately differ on macOS and are not behavior under test.
      .replace(/\bPlatform: (?:linux|macos|darwin|win32)\b/g, "Platform: <platform>")
      .replace(/\bCommands run on (?:Linux|macOS|Windows) using (?:bash|zsh|sh|fish|pwsh|powershell)\b/g, "Commands run on <platform> using <shell>")
      // The sandbox root's absolute length differs by platform, so a path component the TUI ellipsizes on one
      // platform can be shown whole on another. Controlled-name scenarios cover truncation itself.
      .replace(/[^\s"]*<root-name>\/<project-name>/g, "<project>")
      // A project directory the longer macOS root truncates before `<project-name>`.
      .replace(/[^\s"]*<root-name>\/project[^:\s"]*/g, "<project>")
      // A longer sandbox root can truncate the project path before its `:branch` suffix.
      .replace(/<project>:[^\s"]+/g, "<project>")
      // Padding after the project path varies with the sandbox's absolute length; the columns are layout.
      .replace(/<project>([^ \t\n]*)[ \t]+/g, "<project>$1 ")
      .replaceAll(TMP_PARENT_ENCODED, "%2Ftmp")
      .replaceAll(TMP_PARENT, "/tmp")
  if (typeof value === "number" && PID_KEY.test(key)) return "<pid>"
  if (Array.isArray(value)) return value.map((item) => lenient(item, key))
  if (value && typeof value === "object")
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, lenient(v, k)]))
  return value
}
