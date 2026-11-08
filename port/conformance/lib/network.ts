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
import http from "node:http"
import https from "node:https"
import net from "node:net"
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
  // node:http rather than Bun.serve: a failure must reach the client as a closed connection, which needs the
  // socket. A handler that throws stands for a connection that fails before any response (the client sees it
  // close); a response body that errors stands for one that drops while the response is read.
  const serve = (scheme: "http" | "https") => async (req: http.IncomingMessage, res: http.ServerResponse) => {
    const chunks: Buffer[] = []
    for await (const chunk of req) chunks.push(chunk as Buffer)
    const target = req.url?.startsWith("http") ? req.url : `${scheme}://${req.headers.host}${req.url}`
    const headers = new Headers()
    for (const [key, value] of Object.entries(req.headers))
      for (const item of Array.isArray(value) ? value : value === undefined ? [] : [value]) headers.append(key, item)
    const body = req.method === "GET" || req.method === "HEAD" ? undefined : Buffer.concat(chunks)
    const response = await handler(new Request(target, { method: req.method, headers, body })).catch(() => undefined)
    if (!response) return req.socket.destroy()
    res.writeHead(response.status, response.statusText, [...response.headers].flat())
    try {
      // Each chunk is handed to the socket before the next is read, so what was sent before a failure arrives.
      if (response.body)
        for await (const chunk of response.body) await new Promise<void>((resolve) => res.write(chunk, () => resolve()))
      res.end()
    } catch {
      res.socket?.destroy()
    }
  }
  const listen = async (server: http.Server) => {
    // Scripted responses may wait on a scenario (an `until` gate) for as long as it takes.
    server.requestTimeout = 0
    server.headersTimeout = 0
    server.timeout = 0
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve))
    return (server.address() as net.AddressInfo).port
  }
  const servers: http.Server[] = []
  const plain = http.createServer(serve("http"))
  servers.push(plain)
  const plainPort = await listen(plain)
  const secure = new Map<string, Promise<number>>()
  const tlsPort = (host: string) => {
    let port = secure.get(host)
    if (!port) {
      const server = https.createServer(leaf(host), serve("https"))
      servers.push(server)
      port = listen(server)
      secure.set(host, port)
    }
    return port
  }

  const sockets = new Set<net.Socket>()
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
        socket.destroy()
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
      for (const server of servers) {
        server.closeAllConnections()
        server.close()
      }
    },
  }
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
