// Differential oracle for src/numconv.rs: generates cases with a fixed seed and prints, as JSON lines,
// what bun (JavaScriptCore + ICU) computes for each. examples/numconv_check.rs recomputes them.
//
//   bun oracle.ts <function> <random-count> [seed]
//
// Functions: toString toStringRadix toFixed toExponential toPrecision toLocaleString stringToNumber
// parseInt parseFloat toInt32 toUint32 indexToString. Each prints the hand-picked edge cases first
// (every digits/precision/radix value for the edge doubles), then <random-count> random cases.
//
// Line shapes (doubles travel as 16 hex digits of their IEEE bits, strings as 4 hex digits per UTF-16
// code unit, parser results as bits or "NaN"):
//   {"f":"toFixed","x":"3ff0000000000000","d":2,"r":"1.00"}
//   {"f":"toExponential","x":"...","d":null,"r":"1e+0"}
//   {"f":"toStringRadix","x":"...","d":16,"r":"..."}
//   {"f":"parseInt","s":"0031","d":"7ff8000000000000","r":"3ff0000000000000"}

const [fn = "", countArg = "100000", seedArg = "1"] = process.argv.slice(2);
const count = Number(countArg);
let seed = Number(seedArg) >>> 0;

// ---------------------------------------------------------------- PRNG (sfc32)
let sa = 0x9e3779b9 ^ seed, sb = 0x243f6a88 + seed, sc = 0xb7e15162 ^ (seed * 7), sd = 1;
function u32(): number {
  sa >>>= 0; sb >>>= 0; sc >>>= 0; sd >>>= 0;
  const t = (sa + sb + sd) >>> 0;
  sd = (sd + 1) >>> 0;
  sa = sb ^ (sb >>> 9);
  sb = (sc + (sc << 3)) >>> 0;
  sc = (sc << 21) | (sc >>> 11);
  sc = (sc + t) >>> 0;
  return t;
}
for (let i = 0; i < 20; i++) u32();
const below = (n: number) => Math.floor((u32() / 4294967296) * n);
const chance = (p: number) => u32() / 4294967296 < p;
const pick = <T,>(xs: readonly T[]): T => xs[below(xs.length)];

// ---------------------------------------------------------------- bits helpers
const dv = new DataView(new ArrayBuffer(8));
function hex(x: number): string {
  dv.setFloat64(0, x);
  return dv.getUint32(0).toString(16).padStart(8, "0") + dv.getUint32(4).toString(16).padStart(8, "0");
}
function fromBits(hi: number, lo: number): number {
  dv.setUint32(0, hi >>> 0);
  dv.setUint32(4, lo >>> 0);
  return dv.getFloat64(0);
}
function nextUp(x: number): number {
  if (Number.isNaN(x) || x === Infinity) return x;
  if (x === 0) return 5e-324;
  dv.setFloat64(0, x);
  let hi = dv.getUint32(0), lo = dv.getUint32(4);
  if (x > 0) { lo = (lo + 1) >>> 0; if (lo === 0) hi = (hi + 1) >>> 0; }
  else { if (lo === 0) hi = (hi - 1) >>> 0; lo = (lo - 1) >>> 0; }
  return fromBits(hi, lo);
}
const nextDown = (x: number) => -nextUp(-x);
const resultBits = (x: number) => (Number.isNaN(x) ? "NaN" : hex(x));
function hexUnits(s: string): string {
  let out = "";
  for (let i = 0; i < s.length; i++) out += s.charCodeAt(i).toString(16).padStart(4, "0");
  return out;
}

// ---------------------------------------------------------------- output
const sink = Bun.stdout.writer();
let pending: string[] = [];
function emit(line: string) {
  pending.push(line);
  if (pending.length >= 4096) flush();
}
function flush() {
  if (pending.length) sink.write(pending.join("\n") + "\n");
  pending = [];
}

// ---------------------------------------------------------------- double generators
function digitString(n: number, radix = 10): string {
  let s = "";
  for (let i = 0; i < n; i++) s += below(radix).toString(radix);
  return s;
}
function niceDecimal(): number {
  const digits = 1 + below(17);
  let s = String(1 + below(9)) + digitString(digits - 1);
  const e = below(61) - 30;
  return Number(`${chance(0.3) ? "-" : ""}${s}e${e}`);
}
function shortDecimal(): number {
  // The kind of number programs print: few digits, few decimals.
  const intPart = chance(0.5) ? below(100) : below(10 ** (1 + below(9)));
  const decimals = below(7);
  const s = `${intPart}.${digitString(decimals)}`;
  return (chance(0.2) ? -1 : 1) * Number(s);
}
function randomBits(): number {
  return fromBits(u32(), u32());
}
function moderate(): number {
  // A random mantissa with an exponent between about 1e-25 and 1e25.
  const exp = 1023 - 84 + below(168);
  return fromBits(((chance(0.25) ? 0x80000000 : 0) | (exp << 20) | below(1 << 20)) >>> 0, u32());
}
function integer(): number {
  const bits = 1 + below(70);
  let v = 0;
  for (let i = 0; i < bits; i++) v = v * 2 + (u32() & 1);
  return chance(0.3) ? -v : v;
}
function tie(): number {
  // Exactly representable halfway cases for decimal rounding: n + odd/2^j, and decimal ties like x.5, x.25.
  const j = 1 + below(12);
  const n = chance(0.5) ? below(1000) : below(2 ** (1 + below(40)));
  const frac = (2 * below(2 ** (j - 1)) + 1) / 2 ** j;
  return (chance(0.2) ? -1 : 1) * (n + frac);
}
function subnormal(): number {
  return fromBits((chance(0.5) ? 0x80000000 : 0) | below(1 << 20), u32());
}
function unit(): number {
  return (u32() * 2 ** 21 + (u32() >>> 11)) / 2 ** 53;
}
function neighbour(): number {
  const x = chance(0.5) ? niceDecimal() : shortDecimal();
  const k = 1 + below(3);
  let y = x;
  for (let i = 0; i < k; i++) y = chance(0.5) ? nextUp(y) : nextDown(y);
  return y;
}
// Decimal strings that sit on a rounding midpoint, or a hair off it, after `digits` significant
// digits (significant = true) or fraction digits: where the shortest digits end in a 5 but the
// double is slightly above or below.
function nearTie(digits: number, significant: boolean): number {
  const tail = pick(["5", "5", "49", "51", "4" + "9".repeat(1 + below(14)), "5" + "0".repeat(below(14)) + "1", "50", "45", "55"]);
  let s: string;
  if (significant) {
    const head = String(1 + below(9)) + digitString(Math.max(0, digits - 1));
    s = `${head}${tail}e${below(60) - 30 - head.length}`;
  } else {
    const ip = chance(0.3) ? "0" : String(below(10 ** (1 + below(12))));
    s = `${ip}.${digitString(digits)}${tail}`;
  }
  return (chance(0.3) ? -1 : 1) * Number(s);
}
function anyDouble(): number {
  switch (below(12)) {
    case 0: case 1: return randomBits();
    case 2: case 3: return moderate();
    case 4: return niceDecimal();
    case 5: case 6: return shortDecimal();
    case 7: return integer();
    case 8: return tie();
    case 9: return subnormal();
    case 10: return unit() * (chance(0.5) ? 1 : 10 ** (below(8)));
    default: return neighbour();
  }
}

function edgeDoubles(): number[] {
  const xs = new Set<number>();
  const add = (x: number) => { xs.add(x); xs.add(-x); };
  const around = (x: number) => { add(x); add(nextUp(x)); add(nextDown(x)); };
  for (const x of [0, NaN, Infinity, 5e-324, 1e-323, 2.2250738585072014e-308, 2.225073858507201e-308,
    Number.MAX_VALUE, Number.EPSILON, 1e21, 1e-7, 1e-6, 2 ** 53, 2 ** 31, 2 ** 32, 2 ** 63, 2 ** 64,
    0.1, 0.2, 0.3, 0.5, 1.5, 2.5, 1.005, 1.0005, 1.0015, 1.0025, 0.000001, 123.456, 0.1 + 0.2,
    999.9995, 9.9995, 0.0005, 0.0015, 1234.5675, 4.35, 1.45, 8.345, 1.255, 10.235, 0.125, 0.375,
    2 ** 50 + 0.25, 2 ** 50 + 0.75, 2 ** 52 - 0.5, 2 ** 51 + 0.5, 123456789012345680000, 1e300,
    Math.PI, Math.E, Math.SQRT2, 1 / 3, 2 / 3, 4294967295.5, 2147483647.5, 1e15, 1e16, 1e17,
    999999999999999900000, 0.000001234, 1.7976931348623157e308 / 3, 5e-7, 1.5e-7, 9.5e-7, 1e100]) around(x);
  for (let e = -1074; e <= 1023; e++) around(2 ** e);
  for (let e = -323; e <= 308; e++) around(Number(`1e${e}`));
  for (let e = -20; e <= 22; e++) { around(Number(`5e${e}`)); around(Number(`2.5e${e}`)); around(Number(`9.5e${e}`)); }
  for (let i = 0; i <= 200; i++) { add(i); add(i / 8); add(i + 0.5); add(i / 1000); }
  return [...xs];
}

// ---------------------------------------------------------------- string generators
const WS = [0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004,
  0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000, 0xfeff];
const NOT_WS = [0x180e, 0x200b, 0x85, 0x1c, 0x1f, 0x00, 0x2060, 0xfffe, 0x200c, 0x0e];
function ws(): string {
  if (chance(0.6)) return "";
  let s = "";
  const n = 1 + below(3);
  for (let i = 0; i < n; i++) {
    if (chance(0.5)) s += " ";
    else if (chance(0.95)) s += String.fromCharCode(pick(WS));
    else s += String.fromCharCode(pick(NOT_WS));
  }
  return s;
}
function randomUnit(): number {
  switch (below(5)) {
    case 0: return 0xd800 + below(0x800); // surrogates, often lone
    case 1: return below(0x80);
    case 2: return pick(WS);
    case 3: return pick(NOT_WS);
    default: return below(0x10000);
  }
}
function junk(): string {
  if (chance(0.6)) return "";
  const n = 1 + below(4);
  let s = "";
  for (let i = 0; i < n; i++) {
    if (chance(0.5)) s += pick(["e", "E", ".", "+", "-", "_", "x", "0", "5", "a", "I", "n", " ", ",", "e5", "e+", "Infinity"]);
    else s += String.fromCharCode(randomUnit());
  }
  return s;
}
function sign(): string {
  return chance(0.6) ? "" : pick(["+", "-", "-", "+-", "--", "-+"]);
}
function longDigits(): string {
  const n = chance(0.9) ? 1 + below(25) : chance(0.9) ? 25 + below(400) : 400 + below(3000);
  let s = digitString(n);
  if (chance(0.2)) s = "0".repeat(1 + below(30)) + s;
  return s;
}
function decimalLiteral(): string {
  let s = sign();
  const shape = below(6);
  if (shape === 0) s += "." + longDigits();
  else if (shape === 1) s += longDigits() + ".";
  else if (shape <= 3) s += longDigits() + "." + longDigits();
  else s += longDigits();
  if (chance(0.4)) s += pick(["e", "E"]) + pick(["", "+", "-"]) + (chance(0.95) ? String(below(400)) : chance(0.5) ? "" : digitString(1 + below(30)));
  return s;
}
function halfwayLiteral(): string {
  // The exact midpoint between a double and its successor (and a hair above/below), in decimal.
  let x = Math.abs(chance(0.5) ? moderate() : chance(0.5) ? randomBits() : niceDecimal());
  if (!Number.isFinite(x) || x === 0) x = 1.5;
  dv.setFloat64(0, x);
  const bits = (BigInt(dv.getUint32(0)) << 32n) | BigInt(dv.getUint32(4));
  const expField = Number((bits >> 52n) & 0x7ffn);
  let m = bits & 0xfffffffffffffn;
  let e: number;
  if (expField === 0) e = -1074; else { m |= 1n << 52n; e = expField - 1075; }
  const mid = 2n * m + 1n; // midpoint = mid * 2^(e-1)
  let digits: string, exp: number;
  if (e - 1 >= 0) { digits = (mid << BigInt(e - 1)).toString(); exp = 0; }
  else { const k = 1 - e; digits = (mid * 5n ** BigInt(k)).toString(); exp = -k; }
  const tweak = below(3);
  if (tweak === 1) {
    // A hair above: N * 10^k + 1, scaled back by 10^k.
    const k = 1 + below(5);
    digits += "0".repeat(k - 1) + "1";
    exp -= k;
  } else if (tweak === 2) {
    // A hair below: N * 10^k - 1, scaled back by 10^k.
    const k = 1 + below(5);
    digits = (BigInt(digits) - 1n).toString() + "9".repeat(k);
    exp -= k;
  }
  return `${chance(0.3) ? "-" : ""}${digits}e${exp}`;
}
function radixLiteral(): string {
  const [prefix, radix] = pick([["0x", 16], ["0X", 16], ["0o", 8], ["0O", 8], ["0b", 2], ["0B", 2]] as const);
  const n = chance(0.8) ? 1 + below(16) : 16 + below(120);
  let body = digitString(n, radix);
  if (radix === 16 && chance(0.5)) body = body.toUpperCase();
  if (chance(0.1)) body = "";
  return sign() + prefix + body;
}
function infinityLiteral(): string {
  return sign() + pick(["Infinity", "Infinity", "infinity", "INFINITY", "Infinit", "Infinityy", "Inf", "NaN"]);
}
function numericString(): string {
  let body: string;
  switch (below(10)) {
    case 0: case 1: case 2: case 3: body = decimalLiteral(); break;
    case 4: body = radixLiteral(); break;
    case 5: body = infinityLiteral(); break;
    case 6: body = halfwayLiteral(); break;
    case 7: body = to_string_any(anyDouble()); break;
    case 8: {
      let s = "";
      const n = below(6);
      for (let i = 0; i < n; i++) s += String.fromCharCode(chance(0.5) ? pick([0x30, 0x31, 0x2e, 0x65, 0x2b, 0x2d, 0x78, 0x20]) : randomUnit());
      body = s;
      break;
    }
    default: body = String(Math.trunc(anyDouble() || 0)); break;
  }
  return ws() + body + ws() + junk();
}
function to_string_any(x: number): string {
  switch (below(4)) {
    case 0: return x.toExponential();
    case 1: return Number.isFinite(x) && Math.abs(x) < 1e21 ? x.toFixed(below(30)) : String(x);
    default: return String(x);
  }
}
function parseIntString(radix: number): string {
  let body: string;
  const r = radix >= 2 && radix <= 36 ? radix : pick([10, 16, 2, 36]);
  switch (below(6)) {
    case 0: body = numericString(); break;
    case 1: body = (chance(0.5) ? "0x" : "0X") + digitString(1 + below(40), 16); break;
    default: {
      const n = chance(0.7) ? 1 + below(20) : chance(0.8) ? 20 + below(100) : 100 + below(1200);
      body = digitString(n, r);
      if (r > 10 && chance(0.5)) body = body.toUpperCase();
      break;
    }
  }
  return ws() + sign() + body + junk();
}
function parseIntRadix(): number {
  if (chance(0.2)) return NaN; // undefined
  if (chance(0.7)) return 2 + below(35);
  return pick([0, -0, 1, 37, 36, 2, 16, 10, -1, -16, 16.9, 2.5, 36.99, 37.5, -0.5, 1.5, 2 ** 32 + 16, 2 ** 32 + 10, -(2 ** 32) + 8,
    2 ** 31 + 16, -(2 ** 31), Infinity, -Infinity, 1e10, 4294967312.7, 0.999]);
}

const EDGE_STRINGS = ["", " ", "1.", ".5", "-.5", "+.5", ".", "1e", "1e+", "1e5x", "e5", "-", "+", "infinity", "Infinity",
  "-Infinity", "+Infinity", "INFINITY", "0x", "0x1", "-0x1", "0X1F", "0o17", "0O17", "0b101", "0B101", "0b2", "0x1g",
  "1_000", "00012", "-0", "+-1", "1e1000", "1e-1000", "0.0e0", " 12 ", " 12﻿", "0x 1", "Infinityx", "1.5e+3",
  "1.5E-3", "0xFFFFFFFFFFFFFFFFFFFF", "0x1fffffffffffff1", "0b" + "1".repeat(60), "0o" + "7".repeat(30), ".e5", "5.e5",
  "-.e5", "0.", "-0.", "0", "5", "a", "\t", "\ud800", "\udc00", "1\ud800", "\ud8001", "0x\ud800", "1e00000000000000000000001",
  "1e-00000000000000000000400", "0e99999999999999999999", "9".repeat(400), "1" + "0".repeat(309), "4.9406564584124654e-324",
  "2.4703282292062327e-324", "2.4703282292062328e-324", "1.7976931348623158e308", "1.7976931348623159e308",
  "9007199254740993", "9007199254740993.0000000001", "0x20000000000001", "0x20000000000003", "0b100000000000000000000000000000000000000000000000000001",
  "  -0x10", "0x10  ", "᠎1", "1᠎", "​1", "1\u0085", "08", "0b", "0o", "0b1 ", "0o8", "00x1", "-Infinity ", " +Infinity\n",
  "1.e1", "1.E+1", ".1e-1", "+.1e+1", "1e+-1", "1ee1", "1..1", "1.1.1", "١", "１", "0x١", "12abc", "abc12", "+", "++1"];

// ---------------------------------------------------------------- the functions
type Gen = () => void;
const doubleCase = (f: string, x: number, d: number | null | string, r: string) =>
  emit(`{"f":"${f}","x":"${hex(x)}","d":${d === null ? "null" : typeof d === "string" ? JSON.stringify(d) : d},"r":${JSON.stringify(r)}}`);
const stringCase = (f: string, s: string, d: string | null, r: number) =>
  emit(`{"f":"${f}","s":"${hexUnits(s)}","d":${d === null ? "null" : JSON.stringify(d)},"r":"${resultBits(r)}"}`);

const edges = edgeDoubles();
const gens: Record<string, Gen> = {
  toString() {
    for (const x of edges) doubleCase("toString", x, null, String(x));
    for (let i = 0; i < count; i++) { const x = anyDouble(); doubleCase("toString", x, null, String(x)); }
  },
  toStringRadix() {
    for (const x of edges) for (let r = 2; r <= 36; r++) doubleCase("toStringRadix", x, r, x.toString(r));
    for (let i = 0; i < count; i++) {
      // Random bit patterns are mostly tiny or huge (long outputs); weight them down here.
      const x = chance(0.1) ? randomBits() : anyDouble();
      const r = 2 + (i % 35);
      doubleCase("toStringRadix", x, r, x.toString(r));
    }
  },
  toFixed() {
    for (const x of edges) for (let d = 0; d <= 100; d++) doubleCase("toFixed", x, d, x.toFixed(d));
    for (let i = 0; i < count; i++) {
      const d = chance(0.5) ? below(101) : below(25);
      const x = chance(0.25) ? nearTie(Math.min(d, 20), false) : anyDouble();
      doubleCase("toFixed", x, d, x.toFixed(d));
    }
  },
  toExponential() {
    for (const x of edges) {
      doubleCase("toExponential", x, null, x.toExponential());
      for (let d = 0; d <= 100; d++) doubleCase("toExponential", x, d, x.toExponential(d));
    }
    for (let i = 0; i < count; i++) {
      const d = chance(0.1) ? null : chance(0.5) ? below(101) : below(25);
      const x = d !== null && chance(0.25) ? nearTie(d + 1, true) : anyDouble();
      doubleCase("toExponential", x, d, d === null ? x.toExponential() : x.toExponential(d));
    }
  },
  toPrecision() {
    for (const x of edges) for (let p = 1; p <= 100; p++) doubleCase("toPrecision", x, p, x.toPrecision(p));
    for (let i = 0; i < count; i++) {
      const p = chance(0.5) ? 1 + below(100) : 1 + below(25);
      const x = chance(0.25) ? nearTie(p, true) : anyDouble();
      doubleCase("toPrecision", x, p, x.toPrecision(p));
    }
  },
  toLocaleString() {
    for (const x of edges) doubleCase("toLocaleString", x, null, x.toLocaleString("en-US"));
    for (let i = 0; i < count; i++) {
      let x = anyDouble();
      if (chance(0.15)) x = Math.round(x * 1e4) / 1e4 + (chance(0.5) ? 0.0005 : 0.0015) * (chance(0.5) ? 1 : -1);
      else if (chance(0.3)) x = nearTie(3, false);
      doubleCase("toLocaleString", x, null, x.toLocaleString("en-US"));
    }
  },
  toInt32() {
    for (const x of edges) doubleCase("toInt32", x, null, String(x | 0));
    for (let i = 0; i < count; i++) { const x = anyDouble(); doubleCase("toInt32", x, null, String(x | 0)); }
  },
  toUint32() {
    for (const x of edges) doubleCase("toUint32", x, null, String(x >>> 0));
    for (let i = 0; i < count; i++) { const x = anyDouble(); doubleCase("toUint32", x, null, String(x >>> 0)); }
  },
  indexToString() {
    const one = (i: number) => emit(`{"f":"indexToString","x":"${hex(i)}","d":null,"r":"${String(i)}"}`);
    for (let i = 0; i <= 1000; i++) one(i);
    for (let e = 0; e <= 53; e++) { one(2 ** e); one(2 ** e - 1); }
    for (let i = 0; i < count; i++) one(Math.floor(unit() * 2 ** (1 + below(53))));
  },
  stringToNumber() {
    const one = (s: string) => stringCase("stringToNumber", s, null, Number(s));
    for (const s of EDGE_STRINGS) one(s);
    for (let c = 0; c < 0x10000; c++) { const ch = String.fromCharCode(c); one(ch); one(ch + "1"); one("1" + ch); one(ch + "1" + ch); }
    for (let i = 0; i < count; i++) one(numericString());
  },
  parseFloat() {
    const one = (s: string) => stringCase("parseFloat", s, null, parseFloat(s));
    for (const s of EDGE_STRINGS) one(s);
    for (let c = 0; c < 0x10000; c++) { const ch = String.fromCharCode(c); one(ch); one(ch + "1"); one("1" + ch); one(ch + ".5" + ch); }
    for (let i = 0; i < count; i++) one(numericString());
  },
  parseInt() {
    const one = (s: string, r: number) =>
      stringCase("parseInt", s, hex(r), Number.isNaN(r) ? parseInt(s) : parseInt(s, r));
    const radices = [NaN, 0, -0, 1, 2, 3, 7, 8, 10, 16, 32, 36, 37, -1, 16.5, 2 ** 32 + 16, -(2 ** 32) + 10, Infinity];
    for (const s of EDGE_STRINGS) for (const r of radices) one(s, r);
    for (let c = 0; c < 0x10000; c++) { const ch = String.fromCharCode(c); one(ch, NaN); one(ch + "1", NaN); one("1" + ch, 36); one(ch + "z", 36); }
    for (let r = 2; r <= 36; r++) for (let n = 1; n <= 120; n++) {
      one("1".repeat(n), r); one((r - 1).toString(r).repeat(n), r); one("1" + "0".repeat(n), r);
    }
    for (let i = 0; i < count; i++) { const r = parseIntRadix(); one(parseIntString(r), r); }
  },
};

const g = gens[fn];
if (!g) {
  console.error(`unknown function ${JSON.stringify(fn)}; one of ${Object.keys(gens).join(" ")}`);
  process.exit(2);
}
g();
flush();
await sink.end();
