// The fake network for implementations that run as separate processes (the TS reference's CLI, the Rust
// build). In-process, the TS server gets the fake OpenRouter as its `fetch` option (lib/scenario.ts); a
// process gets a forward proxy through the standard environment variables instead, which Bun's fetch and
// Rust's HTTP clients honor, so the implementation needs no test-only code path:
//
// - HTTP_PROXY / HTTPS_PROXY (both cases) point at a proxy on 127.0.0.1. For a server process NO_PROXY is
//   empty: every outbound request, loopback included (in-process, the startup probes of local model servers
//   reach the injected fetch too), goes to the scenario's handler. CLI processes reach loopback directly (the
//   servers they start). Nothing reaches the real network.
// - Plain HTTP arrives in absolute form (`GET http://host/path`); the connection is passed to a local server
//   that reads the target from the request line.
// - HTTPS arrives as `CONNECT host:port`; the tunnel is passed to a local TLS server for that host, whose
//   certificate is signed by a CA made for this test process. The CA is trusted through NODE_EXTRA_CA_CERTS
//   (Bun, Node) and SSL_CERT_FILE (OpenSSL, rustls-native-certs), so the requests keep their real origins
//   (`https://openrouter.ai/...`) and are recorded exactly as in-process.
import { spawnSync } from "node:child_process"
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { STATUS_CODES } from "node:http"
import net from "node:net"
import tls from "node:tls"
import path from "node:path"

export interface FakeNetwork {
  /** Environment for an implementation process. */
  readonly env: Record<string, string>
  stop(): void
}

/**
 * `direct`: hosts reached without the proxy (NO_PROXY). CLI processes reach the servers they start on loopback;
 * a server process sends everything, loopback included, to the fake.
 */
export async function fakeNetwork(
  handler: (request: Request) => Promise<Response>,
  options: { readonly direct?: readonly string[] } = {},
): Promise<FakeNetwork> {
  // A minimal HTTP/1.1 server on raw sockets, so the client receives exactly the status line and headers the
  // handler gave (plus the framing: a chunked body, one response per connection), and a failure reaches it as a
  // connection that ends with every byte sent before it delivered. A handler that throws stands for a
  // connection that fails before any response; a response body that errors, for one that drops while the
  // response is read. Ended, not reset: a reset can make the client discard bytes it has received but not
  // read yet.
  const exchange = (scheme: "http" | "https") => (socket: net.Socket) => {
    sockets.add(socket)
    socket.on("close", () => sockets.delete(socket))
    socket.on("error", () => socket.destroy())
    void readRequest(socket).then(async (parsed) => {
      if (!parsed) return socket.end()
      const target = parsed.target.startsWith("http")
        ? parsed.target
        : `${scheme}://${parsed.headers.get("host")}${parsed.target}`
      const bodyless = parsed.method === "GET" || parsed.method === "HEAD"
      const request = new Request(target, {
        method: parsed.method,
        headers: parsed.headers,
        body: bodyless ? undefined : parsed.body,
      })
      const response = await handler(request).catch(() => undefined)
      if (!response) return socket.end()
      const send = (data: string | Uint8Array) => new Promise<void>((resolve) => socket.write(data, () => resolve()))
      const empty = parsed.method === "HEAD" || response.status === 204 || response.status === 304
      const reason = response.statusText || STATUS_CODES[response.status] || ""
      await send(
        [
          `HTTP/1.1 ${response.status} ${reason}`,
          ...[...response.headers].map(([key, value]) => `${key}: ${value}`),
          ...(empty ? [] : ["transfer-encoding: chunked"]),
          "connection: close",
          "",
          "",
        ].join("\r\n"),
      )
      try {
        if (!empty && response.body)
          for await (const chunk of response.body)
            if (chunk.length)
              await send(Buffer.concat([Buffer.from(`${chunk.length.toString(16)}\r\n`), chunk, Buffer.from("\r\n")]))
        if (!empty) await send("0\r\n\r\n")
      } catch {
        // Mid-body: the connection ends before the body does.
      }
      socket.end()
    })
  }
  const listen = async (server: net.Server) => {
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve))
    return (server.address() as net.AddressInfo).port
  }
  const sockets = new Set<net.Socket>()
  const servers: net.Server[] = []
  const plain = net.createServer(exchange("http"))
  servers.push(plain)
  const plainPort = await listen(plain)
  const secure = new Map<string, Promise<number>>()
  const tlsPort = (host: string) => {
    let port = secure.get(host)
    if (!port) {
      const server = tls.createServer(leaf(host), exchange("https"))
      servers.push(server)
      port = listen(server)
      secure.set(host, port)
    }
    return port
  }

  const proxy = net.createServer((socket) => {
    sockets.add(socket)
    socket.on("close", () => sockets.delete(socket))
    socket.on("error", () => socket.destroy())
    let head = Buffer.alloc(0)
    const onData = (chunk: Buffer) => {
      head = Buffer.concat([head, chunk])
      const end = head.indexOf("\r\n\r\n")
      if (end === -1) return
      socket.off("data", onData)
      // Hold what arrives next until the relay is connected (the TLS server may still be starting).
      socket.pause()
      const line = head.subarray(0, head.indexOf("\r\n")).toString("latin1")
      const [method, target] = line.split(" ")
      const tunnel = method === "CONNECT"
      // A tunnel starts after the CONNECT head; a plain request is passed on whole.
      const first = tunnel ? head.subarray(end + 4) : head
      void (tunnel ? tlsPort(hostOf(target)) : Promise.resolve(plainPort)).then((port) => relay(port, tunnel, first))
    }
    const relay = (port: number, tunnel: boolean, first: Buffer) => {
      const upstream = net.connect(port, "127.0.0.1", () => {
        if (tunnel) socket.write("HTTP/1.1 200 Connection Established\r\n\r\n")
        if (first.length) upstream.write(first)
        socket.pipe(upstream)
        upstream.pipe(socket)
      })
      sockets.add(upstream)
      upstream.on("close", () => {
        sockets.delete(upstream)
        socket.end()
      })
      upstream.on("error", () => socket.destroy())
      socket.on("close", () => upstream.destroy())
    }
    socket.on("data", onData)
  })
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve))
  const address = proxy.address() as net.AddressInfo
  const url = `http://127.0.0.1:${address.port}`
  const ca = authority().cert
  return {
    env: {
      HTTP_PROXY: url,
      HTTPS_PROXY: url,
      http_proxy: url,
      https_proxy: url,
      NO_PROXY: (options.direct ?? []).join(","),
      no_proxy: (options.direct ?? []).join(","),
      NODE_EXTRA_CA_CERTS: ca,
      SSL_CERT_FILE: ca,
    },
    stop() {
      proxy.close()
      for (const socket of sockets) socket.destroy()
      for (const server of servers) server.close()
    },
  }
}

/** One request from a client connection: the head, then a body framed by content-length or chunked. */
async function readRequest(socket: net.Socket) {
  let buffer = Buffer.alloc(0)
  let ended = false
  const waiters: (() => void)[] = []
  const wake = () => waiters.splice(0).forEach((resolve) => resolve())
  socket.on("data", (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk])
    wake()
  })
  socket.on("end", () => {
    ended = true
    wake()
  })
  const until = async (ready: () => boolean) => {
    while (!ready()) {
      if (ended) return false
      await new Promise<void>((resolve) => waiters.push(resolve))
    }
    return true
  }
  if (!(await until(() => buffer.includes("\r\n\r\n")))) return undefined
  const end = buffer.indexOf("\r\n\r\n")
  const [line, ...fields] = buffer.subarray(0, end).toString("latin1").split("\r\n")
  buffer = buffer.subarray(end + 4)
  const [method, target] = line.split(" ")
  const headers = new Headers()
  for (const field of fields) {
    const colon = field.indexOf(":")
    if (colon > 0) headers.append(field.slice(0, colon).trim(), field.slice(colon + 1).trim())
  }
  if (headers.get("transfer-encoding")?.toLowerCase().includes("chunked")) {
    const parts: Buffer[] = []
    while (true) {
      if (!(await until(() => buffer.includes("\r\n")))) return undefined
      const size = Number.parseInt(buffer.subarray(0, buffer.indexOf("\r\n")).toString("latin1"), 16)
      const start = buffer.indexOf("\r\n") + 2
      if (!(await until(() => buffer.length >= start + size + 2))) return undefined
      if (size === 0) break
      parts.push(buffer.subarray(start, start + size))
      buffer = buffer.subarray(start + size + 2)
    }
    headers.delete("transfer-encoding")
    return { method, target, headers, body: Buffer.concat(parts) }
  }
  const length = Number(headers.get("content-length") ?? 0)
  if (!(await until(() => buffer.length >= length))) return undefined
  return { method, target, headers, body: buffer.subarray(0, length) }
}

/** `host:port` or `[v6]:port` from a CONNECT target. */
function hostOf(target: string) {
  const bracket = target.match(/^\[([^\]]+)\]/)
  if (bracket) return bracket[1]
  const colon = target.lastIndexOf(":")
  return colon === -1 ? target : target.slice(0, colon)
}

// Certificates live in the test process's sandbox (CONFORMANCE_ROOT), removed with it.
const certDir = () => path.join(process.env.CONFORMANCE_ROOT!, "certs")
const leaves = new Map<string, { cert: string; key: string }>()

function authority() {
  const dir = certDir()
  const cert = path.join(dir, "ca.pem")
  const key = path.join(dir, "ca.key")
  if (!existsSync(cert)) {
    mkdirSync(dir, { recursive: true })
    openssl(
      [
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        key,
        "-out",
        cert,
      ],
      ["-days", "3650", "-subj", "/CN=opencode conformance CA"],
      ["-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign"],
    )
  }
  return { cert, key }
}

function leaf(host: string) {
  const cached = leaves.get(host)
  if (cached) return cached
  const ca = authority()
  const dir = certDir()
  const name = host.replace(/[^A-Za-z0-9.-]/g, "_")
  const key = path.join(dir, `${name}.key`)
  const csr = path.join(dir, `${name}.csr`)
  const cert = path.join(dir, `${name}.pem`)
  const ext = path.join(dir, `${name}.ext`)
  const san = net.isIP(host) ? `IP:${host}` : `DNS:${host}`
  writeFileSync(ext, `subjectAltName=${san}\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n`)
  openssl(
    ["req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", key, "-out", csr],
    ["-subj", `/CN=${host}`],
  )
  openssl(
    ["x509", "-req", "-in", csr, "-CA", ca.cert, "-CAkey", ca.key, "-set_serial", String(Date.now())],
    ["-out", cert, "-days", "365", "-extfile", ext],
  )
  const result = { cert: readFileSync(cert, "utf8"), key: readFileSync(key, "utf8") }
  leaves.set(host, result)
  return result
}

function openssl(...args: string[][]) {
  const result = spawnSync("openssl", args.flat(), { encoding: "utf8" })
  if (result.status !== 0) throw new Error(`openssl ${args.flat().join(" ")} failed: ${result.stderr}`)
}
