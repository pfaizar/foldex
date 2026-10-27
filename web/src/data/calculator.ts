// The launcher's calculator, ported from crates/foldex/src/launcher/calculator.rs
// so the page answers exactly as the app does. A small bounded parser; input
// is never executed as code.

const functions = [
  'sqrt',
  'abs',
  'sin',
  'cos',
  'tan',
  'ln',
  'log',
  'log10',
  'round',
  'floor',
  'ceil',
];

/** Like the launcher's All mode: only answers what is clearly a calculation. */
export function automatic(input: string): string | undefined {
  const text = input.trim();
  if (!text || text.length > 512) return undefined;
  if (text.startsWith('=')) return calculate(text);
  if (dateLike(text)) return undefined;
  const lower = text.toLowerCase();
  const fn = functions.some(
    (name) =>
      lower.startsWith(name) && text.slice(name.length).trimStart()[0] === '(',
  );
  const operation = [...text].some((c, at) => {
    if (c === '+' || c === '-') {
      const left = text.slice(0, at).trimEnd();
      const exponent =
        /[eE]$/.test(left) && /[\d.]/.test(left[left.length - 2] ?? '');
      return !exponent && /[A-Za-z0-9)%π]$/.test(left);
    }
    return '*×/÷^%'.includes(c);
  });
  return fn || operation || convert(text) ? calculate(text) : undefined;
}

function dateLike(text: string) {
  const parts = text.split(/[-/]/);
  return (
    parts.length === 3 &&
    parts.every((part) => /^\d+$/.test(part)) &&
    ((parts[0]!.length === 4 &&
      parts[1]!.length <= 2 &&
      parts[2]!.length <= 2) ||
      (parts[0]!.length <= 2 &&
        parts[1]!.length <= 2 &&
        parts[2]!.length === 4))
  );
}

export function calculate(input: string): string | undefined {
  let text = input.trim();
  if (!text || text.length > 512) return undefined;
  text = text.replace(/^=/, '').replace(/=$/, '').trim();
  const converted = convert(text);
  if (converted) return converted;
  const parser = new Parser(text);
  const value = parser.expression(0);
  parser.space();
  if (value === undefined || parser.at !== text.length || !isFinite(value))
    return undefined;
  return format(value);
}

function format(value: number) {
  if (Object.is(value, -0)) value = 0;
  if (Math.abs(value) >= 1e15 || (value !== 0 && Math.abs(value) < 1e-9))
    return value.toExponential(10);
  return value.toFixed(10).replace(/0+$/, '').replace(/\.$/, '');
}

class Parser {
  at = 0;
  depth = 0;
  constructor(readonly text: string) {}

  space() {
    while (/\s/.test(this.peek() ?? '')) this.at++;
  }
  peek() {
    return this.text[this.at];
  }
  take() {
    return this.text[this.at++];
  }

  expression(min: number): number | undefined {
    if (++this.depth > 32) return undefined;
    this.space();
    const c = this.peek();
    let left: number | undefined;
    if (c === '+' || c === '-') {
      this.take();
      const value = this.expression(3);
      if (value === undefined) return undefined;
      left = c === '-' ? -value : value;
    } else if (c === '(') {
      this.take();
      left = this.expression(0);
      this.space();
      if (left === undefined || this.take() !== ')') return undefined;
    } else if (c !== undefined && /[\d.]/.test(c)) {
      left = this.number();
    } else if (c !== undefined && /[A-Za-zπ]/.test(c)) {
      left = this.identifier();
    }
    if (left === undefined) return undefined;

    for (;;) {
      this.space();
      const op = this.peek();
      if (op === '%') {
        this.take();
        left /= 100;
        continue;
      }
      const precedence =
        op === '+' || op === '-'
          ? 1
          : op === '*' || op === '×' || op === '/' || op === '÷'
            ? 2
            : op === '^'
              ? 4
              : 0;
      if (!precedence || precedence < min) break;
      this.take();
      const right = this.expression(op === '^' ? precedence : precedence + 1);
      if (right === undefined) return undefined;
      if (op === '+') left += right;
      else if (op === '-') left -= right;
      else if (op === '*' || op === '×') left *= right;
      else if (op === '/' || op === '÷') left /= right;
      else left **= right;
      if (!isFinite(left)) return undefined;
    }
    this.depth--;
    return left;
  }

  number() {
    const match = /^[\d.]+(?:[eE][+-]?\d*)?/.exec(this.text.slice(this.at));
    if (!match) return undefined;
    this.at += match[0].length;
    const value = Number(match[0]);
    return isNaN(value) ? undefined : value;
  }

  identifier() {
    const match = /^[A-Za-z0-9π]+/.exec(this.text.slice(this.at))!;
    this.at += match[0].length;
    const name = match[0].toLowerCase();
    if (name === 'pi' || name === 'π') return Math.PI;
    if (name === 'e') return Math.E;
    if (name === 'tau') return Math.PI * 2;
    this.space();
    if (this.take() !== '(') return undefined;
    const value = this.expression(0);
    this.space();
    if (value === undefined || this.take() !== ')') return undefined;
    const fn: Record<string, (x: number) => number> = {
      sqrt: Math.sqrt,
      abs: Math.abs,
      sin: Math.sin,
      cos: Math.cos,
      tan: Math.tan,
      ln: Math.log,
      log: Math.log10,
      log10: Math.log10,
      round: Math.round,
      floor: Math.floor,
      ceil: Math.ceil,
    };
    const result = fn[name]?.(value);
    return result !== undefined && isFinite(result) ? result : undefined;
  }
}

// Kind, scale to the base unit, and offset (for temperatures).
const units: Record<string, [number, number, number]> = {
  mm: [0, 0.001, 0],
  cm: [0, 0.01, 0],
  m: [0, 1, 0],
  meters: [0, 1, 0],
  km: [0, 1000, 0],
  in: [0, 0.0254, 0],
  inches: [0, 0.0254, 0],
  ft: [0, 0.3048, 0],
  feet: [0, 0.3048, 0],
  yd: [0, 0.9144, 0],
  mi: [0, 1609.344, 0],
  miles: [0, 1609.344, 0],
  mg: [1, 0.000001, 0],
  g: [1, 0.001, 0],
  kg: [1, 1, 0],
  lb: [1, 0.45359237, 0],
  lbs: [1, 0.45359237, 0],
  oz: [1, 0.028349523125, 0],
  ms: [2, 0.001, 0],
  s: [2, 1, 0],
  sec: [2, 1, 0],
  min: [2, 60, 0],
  h: [2, 3600, 0],
  hours: [2, 3600, 0],
  days: [2, 86400, 0],
  c: [3, 1, 273.15],
  '°c': [3, 1, 273.15],
  f: [3, 5 / 9, 255.3722222222222],
  '°f': [3, 5 / 9, 255.3722222222222],
  k: [3, 1, 0],
  b: [4, 1, 0],
  kb: [4, 1000, 0],
  mb: [4, 1e6, 0],
  gb: [4, 1e9, 0],
  kib: [4, 1024, 0],
  mib: [4, 1048576, 0],
  gib: [4, 1073741824, 0],
};

function convert(text: string) {
  const tokens = text.split(/\s+/);
  if (tokens.length !== 4 || !/^(to|in)$/i.test(tokens[2]!)) return undefined;
  const value = Number(tokens[0]);
  const from = units[tokens[1]!.toLowerCase()];
  const to = units[tokens[3]!.toLowerCase()];
  if (isNaN(value) || !from || !to || from[0] !== to[0]) return undefined;
  const result = (value * from[1] + from[2] - to[2]) / to[1];
  return isFinite(result) ? `${format(result)} ${tokens[3]}` : undefined;
}
