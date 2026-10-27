// The benchmark race: pick a test and Foldex and File Explorer run it side by
// side. Stopwatches roll as they go, lanes fill, and the chart draws each
// app's progress live. It moves on to the next test by itself until the
// visitor picks one. Rendered on the server too, so the figures read without
// JavaScript.
//
// The motion runs on the compositor (the lanes' fills and the chart's
// curtain); React renders the chart once per test, and only the stopwatches
// update during a race.
import {
  memo,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from 'react';
import { RollingNumber } from '@kitlangton/rolling-number/react';
import '@kitlangton/rolling-number/styles.css';
import { ReferenceLine } from 'recharts';
import { EvilAreaChart } from '@/components/evilcharts/charts/recharts-area-chart';
import type { ChartConfig } from '@/components/evilcharts/ui/recharts-chart';
import type { Benchmark } from '@/data/benchmarks';
import '@/styles/charts.css';

/** How long a race takes on screen, whatever the real times. */
const RACE = 2600;
/** Pause on a finished race before moving on. */
const HOLD = 4200;
const POINTS = 41; // so the six ticks fall on points
/** Height of the time axis under the plot. */
const AXIS = 30;

const config = {
  foldex: {
    label: 'Foldex',
    colors: { light: ['#0a6e8e', '#17a1cc'] },
  },
  explorer: {
    label: 'File Explorer',
    colors: { light: ['#a1a6a9'] },
  },
} satisfies ChartConfig;

const clamp = (v: number) => Math.min(1, Math.max(0, v));
const smooth = (a: number, b: number, v: number) => {
  const t = clamp((v - a) / (b - a));
  return t * t * (3 - 2 * t);
};
// Foldex fills steadily; Explorer stalls and lurches, as it does.
const glide = (u: number) => 1 - Math.pow(1 - clamp(u), 2.4);
const lurch = (u: number) => {
  const v = clamp(u) * 5;
  const k = Math.min(4, Math.floor(v));
  return (k + smooth(0.5, 1, v - k)) / 5;
};

const digits = (unit: Benchmark['unit']) => (unit === 's' ? 1 : 0);
const formatX = (x: number, bench: Benchmark) =>
  bench.curve.kind === 'level'
    ? `${Number(x.toFixed(2))} min`
    : bench.unit === 's'
      ? `${Number(x.toFixed(1))} s`
      : `${Math.round(x)} ms`;
/** A round axis end just past `v`, so five even ticks land on round values. */
const roundUp = (v: number) => {
  const step = 10 ** Math.floor(Math.log10(v)) / 2;
  return Math.ceil(v / step) * step;
};
const TICKS = [0, 8, 16, 24, 32, 40];

/** Each app's reading at `x` (time, or minutes into the session). */
function sample(bench: Benchmark, x: number) {
  const [foldex, explorer] = bench.results.map((r) => r.value) as [
    number,
    number,
  ];
  if (bench.curve.kind === 'level') {
    const span = bench.curve.span;
    const u = x / span;
    return {
      foldex:
        foldex - 5 * Math.exp(-x / 1.4) + 0.7 * Math.sin(x * 2.3) * (1 - u),
      explorer:
        explorer -
        46 * (1 - Math.pow(u, 0.75)) +
        2.6 * Math.sin(x * 1.7) * (1 - u),
    };
  }
  return {
    foldex: 100 * glide(x / foldex),
    explorer: 100 * lurch(x / explorer),
  };
}

function useInView<T extends Element>() {
  const ref = useRef<T>(null);
  const [inView, setInView] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      ([entry]) => setInView(!!entry?.isIntersecting),
      { threshold: 0.3 },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, inView] as const;
}

function usePrefersReducedMotion() {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const query = matchMedia('(prefers-reduced-motion: reduce)');
    setReduced(query.matches);
    const update = () => setReduced(query.matches);
    query.addEventListener('change', update);
    return () => query.removeEventListener('change', update);
  }, []);
  return reduced;
}

type Who = 'foldex' | 'explorer';

/** Where a test's race ends, and its axis. */
function scale(bench: Benchmark) {
  const level = bench.curve.kind === 'level';
  const end =
    bench.curve.kind === 'level' ? bench.curve.span : bench.results[1]!.value;
  return { level, end, xMax: level ? end : roundUp(end * 1.04) };
}

/** How full a lane is at `x`: progress, or memory against Explorer's. */
function fillAt(bench: Benchmark, who: Who, x: number) {
  const v = sample(bench, x)[who];
  return clamp(
    bench.curve.kind === 'level' ? v / bench.results[1]!.value : v / 100,
  );
}

/**
 * One app's lane. The track fills with a compositor animation shaped like the
 * app's curve; only the stopwatch re-renders, a few times a second.
 * `race` is 0 to show the finished result.
 */
function Lane({
  bench,
  who,
  race,
}: {
  bench: Benchmark;
  who: Who;
  race: number;
}) {
  const { level, end } = scale(bench);
  const result = bench.results[who === 'foldex' ? 0 : 1]!;
  // When this app crosses the line, in race time.
  const doneAt = level ? RACE : RACE * Math.min(1, result.value / end);
  const [elapsed, setElapsed] = useState(RACE);
  const fill = useRef<HTMLElement>(null);

  useEffect(() => {
    if (!race) {
      setElapsed(RACE);
      return;
    }
    setElapsed(0);
    const start = performance.now();
    const timer = window.setInterval(
      () => setElapsed(Math.min(performance.now() - start, doneAt)),
      100,
    );
    const finish = window.setTimeout(() => {
      clearInterval(timer);
      setElapsed(RACE);
    }, doneAt);
    const frames = Array.from({ length: 31 }, (_, i) => ({
      transform: `scaleX(${fillAt(bench, who, (i / 30) * end)})`,
    }));
    const animation = fill.current?.animate(frames, {
      duration: RACE,
      fill: 'both',
    });
    return () => {
      clearInterval(timer);
      clearTimeout(finish);
      animation?.cancel();
    };
  }, [race, bench, who, end, doneAt]);

  const x = (elapsed / RACE) * end;
  const done = elapsed >= doneAt;
  const unit = level ? 'MB' : bench.unit;
  const places = digits(unit);
  const reading = level
    ? done
      ? result.value
      : sample(bench, x)[who]
    : Math.min(x, result.value);

  return (
    <div className={`lane lane--${who}`}>
      <span className="lane-name">{result.app}</span>
      <span className="lane-status">
        {level
          ? 'Idle'
          : done
            ? 'Done'
            : who === 'explorer'
              ? 'Working on it…'
              : 'Working…'}
      </span>
      <span className="lane-value">
        <RollingNumber
          value={Math.round(reading * 10 ** places) / 10 ** places}
          locales="en-US"
          format={{
            minimumFractionDigits: places,
            maximumFractionDigits: places,
          }}
          duration={done ? 400 : 120}
        />
        <span className="unit">{unit}</span>
      </span>
      <i className="lane-track">
        <i
          ref={fill}
          style={{ transform: `scaleX(${fillAt(bench, who, end)})` }}
        />
      </i>
    </div>
  );
}

/**
 * The chart, drawn once per test. A curtain over the plot slides away on the
 * compositor in step with the race, so the curves appear to draw live
 * without Recharts re-rendering.
 */
const RaceChart = memo(function RaceChart({
  bench,
  race,
}: {
  bench: Benchmark;
  race: number;
}) {
  const { level, end, xMax } = scale(bench);
  const data = useMemo(
    () =>
      Array.from({ length: POINTS }, (_, i) => {
        const x = (xMax * i) / (POINTS - 1);
        const shown = x <= end + 1e-9;
        const v = sample(bench, Math.min(x, end));
        return {
          at: formatX(x, bench),
          foldex: shown ? Math.round(v.foldex * 10) / 10 : null,
          explorer: shown ? Math.round(v.explorer * 10) / 10 : null,
        };
      }),
    [bench, end, xMax],
  );
  // Where Foldex crossed the line, snapped to the nearest point.
  const finish = level
    ? null
    : data[
        Math.min(
          POINTS - 1,
          Math.ceil((bench.results[0]!.value / xMax) * (POINTS - 1)),
        )
      ]!.at;

  return (
    <figure className="chart">
      <figcaption>
        {bench.curve.label}
        <span>{level ? 'MB' : '%'}</span>
      </figcaption>
      <div className="plot">
        <EvilAreaChart
          data={data}
          config={config}
          className="chart-plot"
          animationType="none"
          curveType="monotone"
          chartProps={{ margin: { top: 8, right: 36, bottom: 0, left: 0 } }}
        >
          <EvilAreaChart.Grid />
          <EvilAreaChart.XAxis
            dataKey="at"
            height={AXIS}
            ticks={TICKS.map((i) => data[i]!.at)}
            interval="equidistantPreserveStart"
          />
          <EvilAreaChart.YAxis
            domain={level ? [0, 'auto'] : [0, 100]}
            ticks={level ? undefined : [0, 50, 100]}
            width={34}
          />
          <EvilAreaChart.Tooltip variant="frosted-glass" roundness="lg" />
          {finish && (
            <ReferenceLine
              x={finish}
              stroke="#17a1cc"
              strokeDasharray="2 3"
              label={{
                value: 'Foldex done',
                position: 'insideTopLeft',
                fill: '#0a6e8e',
                fontSize: 11,
              }}
            />
          )}
          <EvilAreaChart.Area
            dataKey="explorer"
            variant="hatched"
            strokeVariant="dashed"
            strokeWidth={1.4}
          />
          <EvilAreaChart.Area
            dataKey="foldex"
            variant="gradient"
            strokeVariant="solid"
            strokeWidth={2}
          />
        </EvilAreaChart>
        <div
          key={race}
          className={race ? 'scan is-racing' : 'scan'}
          style={
            {
              '--to': end / xMax,
              '--race': `${RACE}ms`,
            } as CSSProperties
          }
          aria-hidden="true"
        >
          <i />
        </div>
      </div>
    </figure>
  );
});

export default function BenchmarkRace({
  benchmarks,
}: {
  benchmarks: Benchmark[];
}) {
  const [ref, inView] = useInView<HTMLDivElement>();
  const reduced = usePrefersReducedMotion();
  const [index, setIndex] = useState(0);
  const [run, setRun] = useState(0);
  const [auto, setAuto] = useState(true);
  const [seen, setSeen] = useState(false);
  // The current race's id; 0 shows finished results (the server render, and
  // reduced motion).
  const [race, setRace] = useState(0);
  const bench = benchmarks[index]!;

  useEffect(() => {
    if (inView) setSeen(true);
  }, [inView]);

  // Start a race whenever the test changes or replays, then move on.
  useEffect(() => {
    if (!seen || reduced || !inView) {
      if (reduced) setRace(0);
      return;
    }
    setRace((r) => r + 1);
    const next = auto
      ? window.setTimeout(
          () => setIndex((i) => (i + 1) % benchmarks.length),
          RACE + HOLD,
        )
      : 0;
    return () => clearTimeout(next);
  }, [index, run, seen, inView, reduced, auto, benchmarks.length]);

  const pick = (i: number) => {
    setAuto(false);
    setIndex(i);
    setRun((r) => r + 1);
  };
  // One decimal below 10×, whole numbers above, as in the headline.
  const ratio = (b: Benchmark) => {
    const r = b.results[1]!.value / b.results[0]!.value;
    return r < 10 ? Math.round(r * 10) / 10 : Math.round(r);
  };

  return (
    <div className="race" ref={ref}>
      <div className="tests" role="group" aria-label="Benchmarks">
        {benchmarks.map((b, i) => (
          <button
            key={b.title}
            type="button"
            className="test"
            aria-pressed={i === index}
            onClick={() => pick(i)}
          >
            <span className="test-title">{b.title}</span>
            <span className="test-times">
              <RollingNumber
                value={seen ? ratio(b) : 0}
                locales="en-US"
                format={{ maximumFractionDigits: 1 }}
                duration={900}
                motionBlur
              />
              <span className="x">×</span>
            </span>
            {i === index && auto && race > 0 && (
              <i
                key={race}
                className="test-timer"
                style={{ animationDuration: `${RACE + HOLD}ms` }}
              />
            )}
          </button>
        ))}
      </div>

      <div className="board">
        <div className="summary">
          <div aria-live="polite">
            <p className="figure">
              <RollingNumber
                value={seen ? ratio(bench) : 0}
                locales="en-US"
                format={{ maximumFractionDigits: 1 }}
                duration={1100}
                motionBlur
              />
              <span className="x">×</span>
            </p>
            <p className="claim">{bench.claim}</p>
            <p className="detail">{bench.detail}</p>
          </div>

          <div className="lanes">
            <Lane key={`f${index}`} bench={bench} who="foldex" race={race} />
            <Lane key={`e${index}`} bench={bench} who="explorer" race={race} />
          </div>
          <button
            type="button"
            className="replay"
            onClick={() => setRun((r) => r + 1)}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M3 8a5 5 0 1 0 1.6-3.7M3 2.5v2.8h2.8" />
            </svg>
            Replay
          </button>
        </div>

        <RaceChart bench={bench} race={race} />
      </div>
    </div>
  );
}
