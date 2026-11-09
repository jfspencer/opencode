// Generates images (PNG and JPEG, opaque and transparent, smooth and noisy, small to over the limits, a few broken
// ones) and records what the reference's PhotonNormalize.normalize (photon-node 0.3.4) makes of each.
//
// usage: bun imagecases.ts <reference core dir> <seed> <count>
const [core, seedArg, countArg] = process.argv.slice(2)
const { PhotonNormalize } = await import(`${core}/src/image/photon-normalize.ts`)
const photon = await PhotonNormalize.load()
const limits = { autoResize: true, maxWidth: 2000, maxHeight: 2000, maxBase64Bytes: 5 * 1024 * 1024 }

let seed = Number(seedArg ?? 1)
const random = () => {
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed / 2147483648
}
const pick = <T>(items: readonly T[]) => items[Math.floor(random() * items.length)]

const shapes: Array<[number, number]> = [
  [1, 1], [16, 16], [255, 257], [300, 300], [700, 500], [2001, 10], [10, 2600], [2600, 90], [2100, 2100],
  [4000, 3], [1, 3000], [900, 900], [1300, 1100], [1024, 768],
]

const image = (width: number, height: number) => {
  const pixels = new Uint8Array(width * height * 4)
  const kind = pick(["gradient", "noise", "blocks", "mixed"])
  const alpha = pick(["opaque", "opaque", "half", "sparse", "full"])
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const at = (y * width + x) * 4
      const noise = () => Math.floor(random() * 256)
      const value =
        kind === "gradient"
          ? [(x * 255) / Math.max(1, width - 1), (y * 255) / Math.max(1, height - 1), ((x + y) * 3) % 256]
          : kind === "noise"
            ? [noise(), noise(), noise()]
            : kind === "blocks"
              ? [((x >> 4) * 40) % 256, ((y >> 4) * 70) % 256, (((x ^ y) >> 3) * 25) % 256]
              : [x % 2 ? noise() : 128, (y * 7) % 256, noise() >> 2]
      pixels[at] = value[0]
      pixels[at + 1] = value[1]
      pixels[at + 2] = value[2]
      pixels[at + 3] =
        alpha === "opaque" ? 255 : alpha === "half" ? (x < width / 2 ? 100 : 255) : alpha === "sparse" ? (random() < 0.03 ? 0 : 255) : 0
    }
  return new photon.PhotonImage(pixels, width, height)
}

const cases = []
const broken = ["iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAf", "/9j/4AAQSkZJRgABAQAAAQABAAD/2Q==", "R0lGODlhAQABAA", "bm90IGFuIGltYWdl"]
for (const data of broken) cases.push({ mime: "image/png", data })
for (let i = 0; i < Number(countArg ?? 40); i++) {
  const [width, height] = pick(shapes)
  const decoded = image(width, height)
  const format = pick(["png", "png", "jpeg"])
  const bytes = format === "png" ? decoded.get_bytes() : decoded.get_bytes_jpeg(pick([95, 80, 50]))
  decoded.free()
  const mime = random() < 0.1 ? pick(["image/webp", "image/png", "image/jpeg"]) : `image/${format}`
  cases.push({ mime, data: Buffer.from(bytes).toString("base64") })
}
const out = cases.map((item) => {
  const result = PhotonNormalize.normalize(photon, { uri: "x", content: item.data, encoding: "base64", mime: item.mime }, limits)
  return { ...item, result: result._tag === "Content" ? { _tag: "Content", mime: result.content.mime, data: result.content.content } : result }
})
console.log(JSON.stringify(out))
