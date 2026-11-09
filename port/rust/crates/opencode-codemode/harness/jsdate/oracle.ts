// The bun side of the jsdate differential check: generates Date cases from fixed seeds, evaluates them with bun's
// JavaScriptCore and prints one JSON array per line. `examples/jsdate_check.rs` recomputes each case with
// `opencode_codemode::jsdate` and reports differences. Run both under the same TZ (see check.sh).
//
// usage: TZ=UTC bun harness/jsdate/oracle.ts [scale] > cases.jsonl     (scale 1 = about 330,000 checked values)
//
// Lines (numbers: JSON numbers, or the strings "NaN", "Infinity", "-Infinity", "-0"):
//   ["g", t, ...17 getter results]                      getters, in jsdate::Getter order
//   ["s", t, setter, [args], result]                    setter index in jsdate::Setter order
//   ["u", [args], result]                               Date.UTC(...args)
//   ["l", [args], result]                               new Date(...args).getTime(), 2..7 args
//   ["c", x, result]                                    new Date(x).getTime()
//   ["f", t, iso|null, utc, date, time, locale, localeDate, localeTime]
//   ["p", string, result]                               Date.parse(string)

const scale = Number(process.argv[2] ?? "1")

function mulberry32(seed: number) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) >>> 0
    let t = a
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const random = mulberry32(0x5eed_da7e)
const int = (lo: number, hi: number) => lo + Math.floor(random() * (hi - lo + 1))
const pick = <T>(items: ReadonlyArray<T>): T => items[Math.floor(random() * items.length)]
const chance = (p: number) => random() < p

const num = (x: number): number | string =>
  Number.isNaN(x) ? "NaN" : x === Infinity ? "Infinity" : x === -Infinity ? "-Infinity" : Object.is(x, -0) ? "-0" : x

const out: string[] = []
let checked = 0
const emit = (row: unknown[], values: number) => {
  out.push(JSON.stringify(row))
  checked += values
  if (out.length >= 4096) flush()
}
const flush = () => {
  if (out.length) process.stdout.write(out.join("\n") + "\n")
  out.length = 0
}

// --- Time values ---

const MAX = 8.64e15
const DAY = 86400000
const YEAR = 365.2425 * DAY

// Offset transitions of the local zone between 1850 and 2100, to the millisecond.
function transitions(): number[] {
  const found: number[] = []
  const step = 6 * 3600000
  let t = Date.UTC(1850, 0, 1)
  const end = Date.UTC(2100, 0, 1)
  let offset = new Date(t).getTimezoneOffset()
  for (; t < end; t += step) {
    const next = new Date(t + step).getTimezoneOffset()
    if (next !== offset) {
      let lo = t
      let hi = t + step
      while (hi - lo > 1) {
        const mid = Math.floor((lo + hi) / 2)
        if (new Date(mid).getTimezoneOffset() === offset) lo = mid
        else hi = mid
      }
      found.push(hi)
      offset = next
    }
  }
  return found
}
const zoneTransitions = transitions()

const edgeTimes = [0, -0, 1, -1, MAX, -MAX, MAX - 1, -MAX + 1, NaN, 946684800000, 951782400000, 1709251200000]

function randomTime(): number {
  const r = random()
  if (r < 0.25) return int(-MAX, MAX)
  if (r < 0.45) return int(-YEAR * 200, YEAR * 150) // 1770..2120
  if (r < 0.65 && zoneTransitions.length) return pick(zoneTransitions) + pick([0, 1, -1, 0, int(-3, 3) * 1800000, int(-86400000, 86400000)])
  if (r < 0.75) {
    // year boundaries, UTC and local
    const y = int(-271820, 275759)
    const base = chance(0.5) ? Date.UTC(y, 0, 1) : new Date(y, 0, 1).getTime()
    return base + int(-2, 2) * pick([1, 1000, 3600000, DAY])
  }
  if (r < 0.85) return int(-YEAR * 3000, YEAR * 3000)
  if (r < 0.9) return pick(edgeTimes)
  return int(-1e12, 1e12) * pick([1, 1000, 60000])
}

const clipped = (x: number) => new Date(x).getTime()

// --- Getters and strings ---

const getterNames = [
  "getFullYear", "getMonth", "getDate", "getDay", "getHours", "getMinutes", "getSeconds", "getMilliseconds",
  "getUTCFullYear", "getUTCMonth", "getUTCDate", "getUTCDay", "getUTCHours", "getUTCMinutes", "getUTCSeconds",
  "getUTCMilliseconds", "getTimezoneOffset",
] as const

function getterCase(t: number) {
  const d = new Date(t)
  emit(["g", num(t), ...getterNames.map((name) => num(d[name]()))], getterNames.length)
}

function stringCase(t: number) {
  const d = new Date(t)
  const o = { timeZone: "UTC" } as const
  const iso = Number.isFinite(t) ? d.toISOString() : null
  emit(
    ["f", num(t), iso, d.toUTCString(), d.toDateString(), d.toTimeString(), d.toLocaleString("en-US", o),
      d.toLocaleDateString("en-US", o), d.toLocaleTimeString("en-US", o)],
    7,
  )
}

// --- Setters, UTC and local construction ---

const setters: ReadonlyArray<readonly [string, number]> = [
  ["setTime", 1], ["setMilliseconds", 1], ["setUTCMilliseconds", 1], ["setSeconds", 2], ["setUTCSeconds", 2],
  ["setMinutes", 3], ["setUTCMinutes", 3], ["setHours", 4], ["setUTCHours", 4], ["setDate", 1], ["setUTCDate", 1],
  ["setMonth", 2], ["setUTCMonth", 2], ["setFullYear", 3], ["setUTCFullYear", 3],
]

const specials = [NaN, Infinity, -Infinity, -0, 0, 0.5, -0.5, 1.5, -1.5, 0.999, -0.999]
const huge = [1e8, -1e8, 1e9, 2 ** 31 - 1, 2 ** 31, -(2 ** 31), 2 ** 32, 1e12, 1e15, -1e15, 8.64e15, 2 ** 53, 1e17, 1e20, -1e20, 1e300, -1e300]
const limits = [275760, 275761, -275760, -275761, -271821, -271822, 3309120, 3309121, -3309120, -3309121, 1e8, 1e8 + 1, -1e8, 2e8]

// A component value of the given kind: "year" | "month" | "date" | "hour" | "minute" | "second" | "ms" | "time".
function component(kind: string): number {
  const r = random()
  if (r < 0.06) return pick(specials)
  if (r < 0.12) return pick(huge) * (chance(0.5) ? 1 : -1)
  if (r < 0.16) return pick(limits)
  if (r < 0.22) return int(-100, 100) + (chance(0.3) ? pick([0.25, 0.5, 0.75, -0.5]) : 0)
  if (r < 0.3) return int(-1e6, 1e6) * (chance(0.5) ? 1 : 1000)
  switch (kind) {
    case "year":
      return chance(0.7) ? int(1900, 2100) : chance(0.5) ? int(0, 99) : int(-271821, 275760)
    case "month":
      return chance(0.9) ? int(0, 11) : int(-30, 40)
    case "date":
      return chance(0.9) ? int(1, 31) : int(-400, 400)
    case "hour":
      return chance(0.9) ? int(0, 23) : int(-50, 50)
    case "minute":
    case "second":
      return chance(0.9) ? int(0, 59) : int(-200, 200)
    case "ms":
      return chance(0.9) ? int(0, 999) : int(-5000, 5000)
    default:
      return randomTime()
  }
}

const setterKinds: Record<string, string[]> = {
  setTime: ["time"],
  setMilliseconds: ["ms"],
  setSeconds: ["second", "ms"],
  setMinutes: ["minute", "second", "ms"],
  setHours: ["hour", "minute", "second", "ms"],
  setDate: ["date"],
  setMonth: ["month", "date"],
  setFullYear: ["year", "month", "date"],
}

function setterCase() {
  const index = int(0, setters.length - 1)
  const [name, length] = setters[index]
  const kinds = setterKinds[name.replace("UTC", "")]
  const count = chance(0.03) ? 0 : int(1, length)
  const args: number[] = []
  for (let i = 0; i < count; i++) args.push(component(kinds[i]))
  // Sometimes compensate a huge component with the next one, landing back in range.
  if (count >= 2 && chance(0.05)) {
    const unit = { setHours: 60, setUTCHours: 60, setMinutes: 60, setUTCMinutes: 60, setSeconds: 1000, setUTCSeconds: 1000 }[name]
    if (unit) {
      args[0] = int(-1, 1) * pick(huge.filter(Number.isFinite))
      args[1] = -args[0] * unit + int(-100, 100)
    }
  }
  const t = chance(0.05) ? NaN : clipped(randomTime())
  const d = new Date(t) as any
  const result = d[name](...args)
  emit(["s", num(t), index, args.map(num), num(result)], 1)
}

const componentKinds = ["year", "month", "date", "hour", "minute", "second", "ms"]

function componentArgs(min: number): number[] {
  const count = int(min, 7)
  const args: number[] = []
  for (let i = 0; i < count; i++) args.push(component(componentKinds[i]))
  if (count >= 3 && chance(0.05)) {
    // Normalization from far-out years or months back into range.
    const y = pick([300000, 1e6, 2 ** 31 - 1, 2 ** 31, -(2 ** 31), -300000, 4e9])
    args[0] = y
    args[1] = chance(0.5) ? 0 : -(y - 1970) * 12
    if (args[1] === 0) args[2] = -Math.round((y - 1970) * 365.2425) + int(-1000, 1000)
  }
  return args
}

function utcCase() {
  const args = componentArgs(0)
  emit(["u", args.map(num), num(Date.UTC(...(args as [number])))], 1)
}

function localCase() {
  let args = componentArgs(2)
  if (chance(0.3) && zoneTransitions.length) {
    // Local times around a transition: skipped and repeated hours.
    const t = pick(zoneTransitions) + int(-4, 4) * 1800000 + pick([0, 0, 1, -1, 60000])
    const d = new Date(t)
    args = [d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCHours(), d.getUTCMinutes(), d.getUTCSeconds(), d.getUTCMilliseconds()]
    const offsetGuess = int(-14, 14) * 1800000
    const local = new Date(t + offsetGuess)
    args = [local.getUTCFullYear(), local.getUTCMonth(), local.getUTCDate(), local.getUTCHours(), local.getUTCMinutes()]
    if (chance(0.5)) args.push(local.getUTCSeconds(), local.getUTCMilliseconds())
  }
  emit(["l", args.map(num), num(new Date(...(args as [number, number])).getTime())], 1)
}

function clipCase() {
  const x = chance(0.3) ? pick([...specials, ...huge, MAX, -MAX, MAX + 1, -MAX - 1, MAX + 0.5, -MAX - 0.5]) : randomTime() + pick([0, 0.5, -0.5, 0.25, 0.999])
  emit(["c", num(x), num(new Date(x).getTime())], 1)
}

// --- Date.parse ---

const curated = [
  "", " ", "\0", "2020", "2020-01", "2020-01-01", "2020-01-01T00:00", "2020-01-01T00:00Z", "2020-01-01T00:00:00",
  "2020-01-01T00:00:00Z", "2020-01-01T00:00:00.000Z", "2020-01-01T00:00:00.1Z", "2020-01-01T00:00:00.12Z",
  "2020-01-01T00:00:00.1239Z", "2020-01-01T00:00:00.0001234567891Z", "2020-01-01T24:00Z", "2020-01-01T24:00:00.000Z",
  "2020-01-01T24:00:00.001Z", "2020-01-01T24:01Z", "2020-01-01T25:00Z", "2020-01-01T00:60Z", "2020-01-01T00:00:60Z",
  "2020-02-29", "2021-02-29", "2020-02-30", "2020-02-31", "2020-02-32", "2020-13-01", "2020-00-01", "2020-01-00",
  "2020-1-1", "2020-01-1", "2020-1-01", "02020-01-01", "+002020-01-01", "-002020-01-01", "+2020-01-01", "-000000-01-01",
  "-000001-01-01", "000000-01-01", "+275760-09-13T00:00:00.000Z", "+275760-09-13T00:00:00.001Z", "-271821-04-20T00:00:00Z",
  "-271821-04-19T23:59:59.999Z", "+275760-09-13T00:00:00", "-271821-04-20T00:00:00", "+275760-09-13", "275760-09-13",
  "2020-01-01T10:00:00+01:00", "2020-01-01T10:00:00-01:00", "2020-01-01T10:00:00+0100", "2020-01-01T10:00:00+01",
  "2020-01-01T10:00:00+24:00", "2020-01-01T10:00:00+23:59", "2020-01-01T10:00:00+2360", "2020-01-01T10:00:00 +01:00",
  "2020-01-01T10:00:00 Z", "2020-01-01T10:00:00z", "2020-01-01t10:00:00Z", "2020-01-01 10:00:00", "2020-01-01 10:00:00Z",
  "2020-01-01 10:00:00 GMT", "2020-01-01 10:00:00 PST", "2020-01-01T10Z", "2020-01-01T10", "2020-01-01T", "T10:00",
  "2020-01-01Z", "2020-01-01 ", " 2020-01-01", "2020-01-01junk", "2020-01-01 junk", "2020-01-01T10:00:00Z junk",
  "2020/01/01", "2020/1/1 10:00", "2020/01/01 10:00:00 GMT+0100", "2020.01.01", "01.01.2020", "1/2/2020", "12/31/2020",
  "12/31/20", "12/31/49", "12/31/50", "12/31/99", "12/31/100", "12/31/0", "1/1/1", "31/12/2020", "0", "1", "12", "13",
  "31", "32", "49", "50", "99", "100", "999", "1000", "9999", "10000", "100000", "275760", "275761", "-1", "+1", "-2020",
  "+2020", "20200601", "1234567890", "Jan", "mar", "Jan 1", "may 5", "Mar 1 2022", "Mar 1, 2022", "1 Mar 2022",
  "2022 Mar 1", "Mar 2022", "Mar 2022 1", "March 1, 2022", "Tuesday, March 1, 2022", "Tue Mar 01 2022",
  "Tue Mar 01 2022 10:00:00 GMT+0100 (Central European Standard Time)", "Tue, 01 Mar 2022 10:00:00 GMT",
  "Tue, 01 Mar 2022 10:00:00 +0000", "Tue, 01 Mar 2022 10:00:00 -0500", "01 Mar 2022 10:00 UT", "01 Mar 22 10:00 EST",
  "1 Mar 22", "1 Mar 49", "1 Mar 50", "1 Mar 99", "1 Mar 100", "01-Mar-2022", "01-Mar-22", "Mar-01-2022", "2022-Mar-01",
  "Mar 1 2022 10:00 PM", "Mar 1 2022 10:00 pm", "Mar 1 2022 12:00 AM", "Mar 1 2022 12:00 PM", "Mar 1 2022 0:00 PM",
  "Mar 1 2022 13:00 PM", "Mar 1 2022 10 PM", "Mar 1 2022 PM 10:00", "Mar 1 2022 10:00 PM PM", "Mar 1 2022 10::",
  "Mar 1 2022 10: PM", "Mar 1 2022 10:00:00.5", "Mar 1 2022 10:00:00.", "Mar 1 2022 10:00:00.123456", "Mar 1 2022 24:00",
  "Mar 1 2022 24:00:01", "Mar 1 2022 23:60", "Mar 1 2022 10:00X", "Mar 1 2022 10:00Z", "Mar 1 2022 10:00-05",
  "Mar 1 2022 10:00EST", "Mar 1 2022 10:00 EST", "Mar 1 2022 10:00 est", "Mar 1 2022 10:00 ESTX", "Mar 1 2022 10:00 E",
  "Mar 1 2022 10:00 EDT", "Mar 1 2022 10:00 CST", "Mar 1 2022 10:00 CDT", "Mar 1 2022 10:00 MST", "Mar 1 2022 10:00 MDT",
  "Mar 1 2022 10:00 PST", "Mar 1 2022 10:00 PDT", "Mar 1 2022 10:00 UT", "Mar 1 2022 10:00 UTC", "Mar 1 2022 10:00 GMT",
  "Mar 1 2022 10:00 Z", "Mar 1 2022 10:00 GMT+5", "Mar 1 2022 10:00 +5", "Mar 1 2022 +5", "Mar 1 2022 UTC+5",
  "Mar 1 2022 GMT+0530", "Mar 1 2022 GMT+530", "Mar 1 2022 GMT+05:30", "Mar 1 2022 GMT+05:", "Mar 1 2022 GMT+",
  "Mar 1 2022 GMT-", "Mar 1 2022 GMT+12345", "Mar 1 2022 GMT+123456789:00", "Mar 1 2022 GMT+596523:00",
  "Mar 1 2022 GMT+596524:00", "Mar 1 2022 GMT+1193047:00", "Mar 1 2022 10:00 GMT+05:30 (India)",
  "Mar 1 2022 10:00 (comment (nested)) GMT", "Mar 1 2022 10:00 (unclosed GMT", "Mar 1 2022 10:00 ) GMT",
  "Mar 1 2022 10:00 garbage", "garbage Mar 1 2022 10:00", "garbage1 Mar 1 2022", "garbage 1 Mar 2022", "1 2 3", "1 2 3 4",
  "2020 1 2", "2020 Jan 32", "32 Jan 2020", "Jan 32 2020", "1 Jan 32", "Jan Feb 1 2020", "1/2/3/4", "1.2.3", "1:2:3",
  "1:2:3:4", "1:2:3.4", "10:00 Jan 1 2020", "10:00:00 2020-01-01", "Jan 1 2020 am", "am Jan 1 2020", "Jan 1 2020 t",
  "mayday 5 2020", "UTC Jan 1 2020", "Jan 1 2020 10:00 +01 +02", "Jan 1 2020 10:00 GMT+01:00:00", "Jan 1 2020, 10:00:00 AM",
  "1/1/2020, 10:00:00 AM", "1/1/1970, 12:00:00 AM", "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)",
  "Thu, 01 Jan 1970 00:00:00 GMT", "Sep 13 275760", "Sep 13 275760 GMT+1", "Sep 13 275760 01:00 GMT+1",
  "Sep 13 275760 00:00 GMT-1", "Apr 20 -271821", "2020-01-01\u0000junk", "Jan 1 2020\u0000junk", "Jan 1 2020",
  "Jan 1 2020", "Jan 1 2020", "Jan 1\u000b2020", "Jan 1\u00852020", "Jan 1　2020", "Jan 1﻿2020",
  "\ud800 2020-01-01", "2020-01-01\ud800", "Jan 1 2020 é", "é Jan 1 2020", "éJan 1 2020", "Ĵan 1 2020",
  "Jan 1 2020 10:00:00 GMT+0000 (é)", "2020-01-01T00:00:00.000+00:00", "1970-01-01T00:00:00.000-00:00",
]

const monthNames = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
const longMonths = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
const weekDays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
const longWeekDays = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
const zones = ["GMT", "UTC", "UT", "Z", "z", "EST", "EDT", "CST", "CDT", "MST", "MDT", "PST", "PDT", "gmt", "est", "CET", "BST", "XYZ"]
const pad = (n: number, w = 2) => String(Math.abs(n)).padStart(w, "0")
const offsetString = (colon: boolean) => {
  const m = int(-14 * 60, 14 * 60)
  return (m < 0 ? "-" : "+") + pad(Math.floor(Math.abs(m) / 60)) + (colon ? ":" : "") + pad(Math.abs(m) % 60)
}

function formatted(): string {
  const t = chance(0.8) ? int(-YEAR * 120, YEAR * 80) : clipped(randomTime())
  const d = new Date(Number.isNaN(t) ? 0 : t)
  const [Y, M, D, h, m, s, ms, wd] = [d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCHours(), d.getUTCMinutes(), d.getUTCSeconds(), d.getUTCMilliseconds(), d.getUTCDay()]
  const yy = pad(Y % 100)
  const year = chance(0.15) ? yy : String(Y)
  const time = () => {
    const r = random()
    if (r < 0.3) return `${pad(h)}:${pad(m)}`
    if (r < 0.7) return `${pad(h)}:${pad(m)}:${pad(s)}`
    if (r < 0.8) return `${pad(h)}:${pad(m)}:${pad(s)}.${pad(ms, int(1, 4))}`
    const h12 = h % 12 === 0 ? 12 : h % 12
    return `${h12}:${pad(m)}${chance(0.7) ? ":" + pad(s) : ""} ${pick(h < 12 ? ["AM", "am", "a.m."] : ["PM", "pm"])}`
  }
  const zone = () => {
    const r = random()
    if (r < 0.3) return ""
    if (r < 0.6) return " " + pick(zones)
    if (r < 0.8) return " " + pick(["GMT", "UTC", ""]) + offsetString(chance(0.3))
    if (r < 0.9) return " " + offsetString(chance(0.5))
    return ` GMT${offsetString(false)} (${pick(["Coordinated Universal Time", "Eastern Standard Time", "x", "(nested)"])})`
  }
  const iso = d.toISOString()
  switch (int(0, 17)) {
    case 0: return iso
    case 1: return iso.slice(0, int(4, iso.length))
    case 2: return iso.replace("Z", chance(0.5) ? offsetString(true) : offsetString(false))
    case 3: return iso.replace("T", " ")
    case 4: return iso.replace("Z", "")
    case 5: return d.toUTCString()
    case 6: return d.toString()
    case 7: return d.toDateString()
    case 8: return d.toLocaleString("en-US", { timeZone: "UTC" })
    case 9: return `${pick([weekDays[wd] + ", ", longWeekDays[wd] + ", ", ""])}${D} ${monthNames[M]} ${year} ${time()}${zone()}`
    case 10: return `${monthNames[M]} ${D} ${year} ${time()}${zone()}`
    case 11: return `${pick(longMonths.slice(M, M + 1))} ${D}, ${year}${chance(0.5) ? " " + time() : ""}${zone()}`
    case 12: return `${Y}/${M + 1}/${D}${chance(0.6) ? " " + time() : ""}${zone()}`
    case 13: return `${M + 1}/${D}/${year}${chance(0.6) ? " " + time() : ""}${zone()}`
    case 14: return `${pad(D)}-${monthNames[M]}-${year}${chance(0.5) ? " " + time() : ""}${zone()}`
    case 15: return `${Y}-${M + 1}-${D}${chance(0.5) ? (chance(0.5) ? "T" : " ") + time() : ""}${zone()}`
    case 16: return `${(Y < 0 ? "-" : "+") + pad(Math.abs(Y), 6)}-${pad(M + 1)}-${pad(D)}${chance(0.5) ? "T" + time().split(" ")[0] + pick(["Z", "", offsetString(true)]) : ""}`
    default: return `${time()} ${monthNames[M]} ${D} ${year}${zone()}`
  }
}

const mutationAlphabet = [
  ..."0123456789", ..."0123456789", ..."-+:./,() ", ..."TZtzAPMampGUTCESDLX", "\t", "\n", "\u000b", " ", " ",
  "　", "﻿", "\u0085", "\u0000", "\ud800", "\udc00", "é", "中", "Jan", "may", "gmt", "UTC", "EST",
  "pm", "(", ")", "((", "  ",
]

function mutated(s: string): string {
  const chars = [...s]
  const edits = int(1, 3)
  for (let i = 0; i < edits; i++) {
    const at = int(0, chars.length)
    const r = random()
    if (r < 0.4) chars.splice(at, 0, pick(mutationAlphabet))
    else if (r < 0.7) chars.splice(at, 1)
    else chars.splice(at, 1, pick(mutationAlphabet))
  }
  return chars.join("")
}

const soupTokens = [
  () => String(int(0, 9)), () => String(int(0, 99)), () => pad(int(0, 99)), () => String(int(0, 9999)),
  () => String(int(0, 10 ** int(1, 12))), () => "0".repeat(int(1, 6)) + int(0, 999), () => pick(monthNames),
  () => pick(longMonths), () => pick(weekDays), () => pick(zones), () => pick(["AM", "PM", "am", "pm"]), () => "T",
  () => pick(["-", "+", ":", ".", "/", ",", "(", ")", " ", " ", " ", "::", "--"]), () => pick(["foo", "x", "junk", "é"]),
]

function soup(): string {
  const n = int(1, 9)
  let s = ""
  for (let i = 0; i < n; i++) s += pick(soupTokens)()
  return s
}

function parseCase(s: string) {
  emit(["p", s, num(Date.parse(s))], 1)
}

// --- Main ---

for (const s of curated) parseCase(s)
for (const t of edgeTimes) {
  getterCase(t)
  stringCase(t)
}
for (const t of zoneTransitions) {
  for (const delta of [-1, 0, 1, -3600000, 3600000]) {
    getterCase(t + delta)
    stringCase(t + delta)
  }
}
const n = Math.round(10000 * scale)
for (let i = 0; i < n; i++) {
  const t = chance(0.03) ? NaN : clipped(randomTime())
  getterCase(t)
  stringCase(t)
  for (let j = 0; j < 6; j++) setterCase()
  utcCase()
  localCase()
  localCase()
  if (i % 2 === 0) clipCase()
  parseCase(formatted())
  parseCase(chance(0.5) ? mutated(formatted()) : mutated(pick(curated)))
  parseCase(soup())
}
flush()
console.error(`oracle: ${checked} checked values, TZ=${process.env.TZ ?? "(unset)"}, ${zoneTransitions.length} zone transitions`)
