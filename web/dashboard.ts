export {};

type NullableNumber = number | null;

const TREND_SAMPLE_INTERVAL_SECONDS = 2;
const HISTORY_FETCH_POINTS = 400;
let lastHistorySeq = -1;
let collecting = false;
let syncCollectingUi: ((value: boolean) => void) | null = null;
let collectionToggleInFlight = false;
let pollRequestInFlight = false;
let zoomStart = 0;
let zoomEnd = 1;
let loadedHistoryBaseSeconds = 0;
const NO_DATA_FONT = "700 20px 'Avenir Next', 'Trebuchet MS', sans-serif";

const delayMs = (ms: number): Promise<void> => new Promise((resolve) => {
  window.setTimeout(resolve, ms);
});

const drawNoData = (ctx: CanvasRenderingContext2D, width: number, height: number): void => {
  ctx.save();
  ctx.font = NO_DATA_FONT;
  ctx.fillStyle = "rgba(230, 241, 255, 0.72)";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("No Data", Math.round(width / 2), Math.round(height / 2));
  ctx.restore();
};

const formatElapsed = (totalSeconds: number): string => {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) {
    return `${h}h ${m}m`;
  }
  if (m > 0) {
    return `${m}m ${s}s`;
  }
  return `${s}s`;
};

type StatusPayload = {
  device: string;
  sensor: {
    ds18b20: {
      name: string;
      temperature_c: NullableNumber;
      temperature_f: NullableNumber;
      error: string;
    };
  };
  pid: {
    target_c: number;
    target_f: number;
    kp: number;
    ki: number;
    kd: number;
    output_percent: number;
    window_step: number;
    on_steps: number;
    relay_on: boolean;
  };
  system: {
    ip: string;
    collecting?: boolean;
    ntp: {
      synced: boolean;
      time: string | null;
      master_address: string | null;
      master_source: string | null;
      master_latency_ms: NullableNumber;
      master_jitter_ms: NullableNumber;
      master_offset_ms: number | null;
      master_offset_jitter_ms: NullableNumber;
    };
    uptime_s: number;
  };
};

type PidSample = {
  target_c: number;
  kp: number;
  ki: number;
  kd: number;
  output_percent: number;
  window_step: number;
  on_steps: number;
  relay_on: number;
};

type HistoryPayload = {
  sample_interval_s: number;
  total_samples: number;
  points: number[][];
};

class Sparkline {
  private readonly canvas: HTMLCanvasElement;
  private readonly values: number[] = [];
  private hoverX: number | null = null;
  private elapsedSeconds: number | null = null;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.canvas.addEventListener("mousemove", (event: MouseEvent) => {
      this.updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this.hoverX = null;
      this.draw();
    });
  }

  setValues(values: number[]): void {
    this.values.length = 0;
    this.values.push(...values);
    this.draw();
  }

  setElapsedSeconds(seconds: number): void {
    this.elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this.draw();
  }

  setHoverRatio(ratio: number | null): void {
    if (ratio === null) {
      if (this.hoverX !== null) {
        this.hoverX = null;
        this.draw();
      }
      return;
    }

    const axisPadLeft = 46;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - 6);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
  }

  private updateHover(clientX: number): void {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
  }

  push(value: number): void {
    this.values.push(value);
    this.draw();
  }

  clear(): void {
    this.values.length = 0;
    this.draw();
  }

  redraw(): void {
    this.draw();
  }

  private draw(): void {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this.values.length < 2) {
      drawNoData(ctx, width, height);
      return;
    }

    const axisPadLeft = 46;
    const plotPadTop = 8;
    const plotPadBottom = 8;
    const plotWidth = Math.max(1, width - axisPadLeft - 6);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);
    const n = this.values.length;
    const visStart = zoomStart * (n - 1);
    const visEnd = zoomEnd * (n - 1);
    const iFirst = Math.max(0, Math.floor(visStart));
    const iLast = Math.min(n - 1, Math.ceil(visEnd));
    const xForIdx = (i: number) => axisPadLeft + ((i - visStart) / Math.max(1, visEnd - visStart)) * plotWidth;
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    for (let i = iFirst; i <= iLast; i++) {
      const v = this.values[i];
      if (v < min) min = v;
      if (v > max) max = v;
    }
    const spread = Math.max(0.1, max - min);

    const yFor = (v: number) => {
      const norm = (v - min) / spread;
      return height - plotPadBottom - norm * plotHeight;
    };

    const axisColor = "rgba(159, 180, 203, 0.35)";
    ctx.strokeStyle = axisColor;
    ctx.lineWidth = 1;

    // Y-axis line
    ctx.beginPath();
    ctx.moveTo(axisPadLeft, plotPadTop);
    ctx.lineTo(axisPadLeft, height - plotPadBottom);
    ctx.stroke();

    // Horizontal guides for max/mid/min
    const tickValues = [max, min + spread / 2, min];
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    tickValues.forEach((tickValue) => {
      const y = yFor(tickValue);
      ctx.strokeStyle = axisColor;
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - 4, y);
      ctx.stroke();
      ctx.fillText(`${tickValue.toFixed(1)} C`, 2, y + 4);
    });

    // X-axis baseline + cumulative elapsed label at far right.
    ctx.beginPath();
    ctx.moveTo(axisPadLeft, height - plotPadBottom);
    ctx.lineTo(width - 4, height - plotPadBottom);
    ctx.stroke();
    const elapsedSeconds = this.elapsedSeconds ?? ((this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
    ctx.save();
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomEnd))}`, width - 4, height - 2);
    if (zoomStart > 0.001) {
      ctx.textAlign = "left";
      ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomStart))}`, axisPadLeft + 4, height - 2);
    }
    ctx.restore();

    const gradient = ctx.createLinearGradient(0, 0, width, 0);
    gradient.addColorStop(0, "#40c4ff");
    gradient.addColorStop(1, "#40d990");

    ctx.save();
    ctx.beginPath();
    ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
    ctx.clip();
    ctx.lineWidth = 2;
    ctx.strokeStyle = gradient;
    ctx.beginPath();
    for (let i = iFirst; i <= iLast; i++) {
      const x = xForIdx(i);
      const y = yFor(this.values[i]);
      if (i === iFirst) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, y);
      }
    }
    ctx.stroke();
    ctx.restore();

    if (this.hoverX !== null && this.values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this.hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const index = Math.max(0, Math.min(n - 1, Math.round(visStart + ratio * (visEnd - visStart))));
      const value = this.values[index];
      const x = clampedX;
      const y = yFor(value);
      const hoverTime = elapsedSeconds * zoomStart + ratio * elapsedSeconds * (zoomEnd - zoomStart);
      const tip = `${value.toFixed(2)} C  T+${formatElapsed(Math.round(hoverTime))}`;

      ctx.save();
      ctx.strokeStyle = "rgba(255,255,255,0.35)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, plotPadTop);
      ctx.lineTo(x, height - plotPadBottom);
      ctx.stroke();

      ctx.fillStyle = "#40d990";
      ctx.beginPath();
      ctx.arc(x, y, 3.5, 0, Math.PI * 2);
      ctx.fill();

      ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
      const paddingX = 8;
      const tipWidth = ctx.measureText(tip).width + paddingX * 2;
      const tipHeight = 20;
      let tipX = x + 10;
      if (tipX + tipWidth > width - 4) {
        tipX = x - tipWidth - 10;
      }
      const tipY = Math.max(4, y - 26);
      ctx.fillStyle = "rgba(6, 12, 20, 0.92)";
      ctx.fillRect(tipX, tipY, tipWidth, tipHeight);
      ctx.strokeStyle = "rgba(130, 184, 235, 0.35)";
      ctx.strokeRect(tipX, tipY, tipWidth, tipHeight);
      ctx.fillStyle = "rgba(230, 241, 255, 0.96)";
      ctx.fillText(tip, tipX + paddingX, tipY + 14);
      ctx.restore();
    }

    if (zoomStart > 0.001 || zoomEnd < 0.999) {
      ctx.save();
      ctx.fillStyle = "rgba(159, 180, 203, 0.15)";
      ctx.fillRect(axisPadLeft, plotPadTop, plotWidth, 3);
      ctx.fillStyle = "rgba(64, 212, 144, 0.5)";
      ctx.fillRect(axisPadLeft + zoomStart * plotWidth, plotPadTop, (zoomEnd - zoomStart) * plotWidth, 3);
      ctx.restore();
    }
  }
}

class PidChart {
  private readonly canvas: HTMLCanvasElement;
  private readonly values: PidSample[] = [];
  private hoverX: number | null = null;
  private elapsedSeconds: number | null = null;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.canvas.addEventListener("mousemove", (event: MouseEvent) => {
      this.updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this.hoverX = null;
      this.draw();
    });
  }

  setValues(values: PidSample[]): void {
    this.values.length = 0;
    this.values.push(...values);
    this.draw();
  }

  setElapsedSeconds(seconds: number): void {
    this.elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this.draw();
  }

  setHoverRatio(ratio: number | null): void {
    if (ratio === null) {
      if (this.hoverX !== null) {
        this.hoverX = null;
        this.draw();
      }
      return;
    }

    const axisPadLeft = 46;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - 6);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
  }

  private updateHover(clientX: number): void {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
  }

  push(sample: PidSample): void {
    this.values.push(sample);
    this.draw();
  }

  clear(): void {
    this.values.length = 0;
    this.draw();
  }

  redraw(): void {
    this.draw();
  }

  private draw(): void {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this.values.length < 2) {
      drawNoData(ctx, width, height);
      return;
    }

    const axisPadLeft = 46;
    const plotPadTop = 8;
    const plotPadBottom = 8;
    const plotWidth = Math.max(1, width - axisPadLeft - 6);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);

    const series = [
      { color: "#f7d774", value: (p: PidSample) => p.target_c },
      { color: "#6ec5ff", value: (p: PidSample) => p.kp },
      { color: "#8ef0c8", value: (p: PidSample) => p.ki },
      { color: "#b28cff", value: (p: PidSample) => p.kd },
      { color: "#ff8d6e", value: (p: PidSample) => p.output_percent },
      { color: "#7cf3ff", value: (p: PidSample) => p.window_step },
      { color: "#ffb3d1", value: (p: PidSample) => p.on_steps },
      { color: "#ffffff", value: (p: PidSample) => p.relay_on },
    ];

    const n = this.values.length;
    const visStart = zoomStart * (n - 1);
    const visEnd = zoomEnd * (n - 1);
    const iFirst = Math.max(0, Math.floor(visStart));
    const iLast = Math.min(n - 1, Math.ceil(visEnd));
    const xForIdx = (i: number) => axisPadLeft + ((i - visStart) / Math.max(1, visEnd - visStart)) * plotWidth;
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    for (let i = iFirst; i <= iLast; i++) {
      const point = this.values[i];
      series.forEach((entry) => {
        const v = entry.value(point);
        if (v < min) min = v;
        if (v > max) max = v;
      });
    }
    const spread = Math.max(0.1, max - min);
    const yFor = (v: number) => {
      const norm = (v - min) / spread;
      return height - plotPadBottom - norm * plotHeight;
    };

    const axisColor = "rgba(159, 180, 203, 0.35)";
    ctx.strokeStyle = axisColor;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(axisPadLeft, plotPadTop);
    ctx.lineTo(axisPadLeft, height - plotPadBottom);
    ctx.stroke();

    const tickValues = [max, min + spread / 2, min];
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    tickValues.forEach((tickValue) => {
      const y = yFor(tickValue);
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - 4, y);
      ctx.stroke();
      ctx.fillText(tickValue.toFixed(1), 2, y + 4);
    });

    ctx.beginPath();
    ctx.moveTo(axisPadLeft, height - plotPadBottom);
    ctx.lineTo(width - 4, height - plotPadBottom);
    ctx.stroke();
    const elapsedSeconds = this.elapsedSeconds ?? ((this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
    ctx.save();
    ctx.textAlign = "right";
    ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomEnd))}`, width - 4, height - 2);
    if (zoomStart > 0.001) {
      ctx.textAlign = "left";
      ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomStart))}`, axisPadLeft + 4, height - 2);
    }
    ctx.restore();

    ctx.save();
    ctx.beginPath();
    ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
    ctx.clip();
    series.forEach((entry, idx) => {
      ctx.beginPath();
      ctx.lineWidth = idx === series.length - 1 ? 1.2 : 1.8;
      ctx.strokeStyle = entry.color;
      for (let i = iFirst; i <= iLast; i++) {
        const x = xForIdx(i);
        const y = yFor(entry.value(this.values[i]));
        if (i === iFirst) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      }
      ctx.stroke();
    });
    ctx.restore();

    if (this.hoverX !== null && this.values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this.hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const i = Math.max(0, Math.min(n - 1, Math.round(visStart + ratio * (visEnd - visStart))));
      const sample = this.values[i];
      const x = clampedX;
      const hoverTime = elapsedSeconds * zoomStart + ratio * elapsedSeconds * (zoomEnd - zoomStart);
      const tip1 = `T+${formatElapsed(Math.round(hoverTime))}`;
      const tip2 = `t:${sample.target_c.toFixed(1)} kp:${sample.kp.toFixed(2)} ki:${sample.ki.toFixed(2)} kd:${sample.kd.toFixed(2)}`;
      const tip3 = `out:${sample.output_percent.toFixed(1)} win:${sample.window_step} on:${sample.on_steps} r:${sample.relay_on}`;

      ctx.save();
      ctx.strokeStyle = "rgba(255,255,255,0.35)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, plotPadTop);
      ctx.lineTo(x, height - plotPadBottom);
      ctx.stroke();

      ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
      const paddingX = 8;
      const tipWidth = Math.max(ctx.measureText(tip1).width, ctx.measureText(tip2).width, ctx.measureText(tip3).width) + paddingX * 2;
      const tipHeight = 52;
      let tipX = x + 10;
      if (tipX + tipWidth > width - 4) {
        tipX = x - tipWidth - 10;
      }
      const tipY = 10;
      ctx.fillStyle = "rgba(6, 12, 20, 0.92)";
      ctx.fillRect(tipX, tipY, tipWidth, tipHeight);
      ctx.strokeStyle = "rgba(130, 184, 235, 0.35)";
      ctx.strokeRect(tipX, tipY, tipWidth, tipHeight);
      ctx.fillStyle = "rgba(230, 241, 255, 0.96)";
      ctx.fillText(tip1, tipX + paddingX, tipY + 14);
      ctx.fillText(tip2, tipX + paddingX, tipY + 28);
      ctx.fillText(tip3, tipX + paddingX, tipY + 42);
      ctx.restore();
    }

    if (zoomStart > 0.001 || zoomEnd < 0.999) {
      ctx.save();
      ctx.fillStyle = "rgba(159, 180, 203, 0.15)";
      ctx.fillRect(axisPadLeft, plotPadTop, plotWidth, 3);
      ctx.fillStyle = "rgba(64, 212, 144, 0.5)";
      ctx.fillRect(axisPadLeft + zoomStart * plotWidth, plotPadTop, (zoomEnd - zoomStart) * plotWidth, 3);
      ctx.restore();
    }
  }
}

const byId = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) {
    throw new Error(`Missing element: ${id}`);
  }
  return el as T;
};

const setText = (id: string, text: string): void => {
  byId<HTMLElement>(id).textContent = text;
};

const formatTemp = (value: NullableNumber, unit: "C" | "F"): string => {
  if (value === null || Number.isNaN(value)) return `--.- ${unit}`;
  return `${value.toFixed(1)} ${unit}`;
};

const formatNumber = (value: NullableNumber, suffix = ""): string => {
  if (value === null || Number.isNaN(value)) return "--";
  return `${value.toFixed(2)}${suffix}`;
};

const formatUptime = (uptimeSec: number): string => {
  const h = Math.floor(uptimeSec / 3600);
  const m = Math.floor((uptimeSec % 3600) / 60);
  const s = uptimeSec % 60;
  return `${h}h ${m}m ${s}s`;
};

let lastUptimeSeconds: number | null = null;

const setTargetFeedback = (text: string, tone: "normal" | "ok" | "error" = "normal"): void => {
  const feedback = byId<HTMLElement>("target-feedback");
  feedback.textContent = text;
  if (tone === "ok") {
    feedback.style.color = "#40d990";
  } else if (tone === "error") {
    feedback.style.color = "#ff6e6e";
  } else {
    feedback.style.color = "";
  }
};

const submitTargetTemperature = async (tempC: number): Promise<void> => {
  const response = await fetch("/temperature", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ temperature_c: tempC }),
  });

  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
};

const clearHistoryOnDevice = async (): Promise<void> => {
  const response = await fetch("/history/clear", { method: "POST" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
};

const setCollectionOnDevice = async (enabled: boolean): Promise<void> => {
  const path = enabled ? "/collection/start" : "/collection/stop";
  // Avoid colliding control requests with in-flight polling on the single-connection HTTP task.
  for (let i = 0; i < 6 && pollRequestInFlight; i += 1) {
    await delayMs(80);
  }
  let lastError: unknown = null;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      const response = await fetch(path, { method: "POST", cache: "no-store" });
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      return;
    } catch (error) {
      lastError = error;
      if (attempt < 3) {
        await delayMs(120 * attempt);
      }
    }
  }
  throw lastError instanceof Error ? lastError : new Error(String(lastError));
};

const loadHistoryFromDevice = async (sparkline: Sparkline, pidChart: PidChart): Promise<void> => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json()) as HistoryPayload;
  const points = Array.isArray(payload.points) ? payload.points : [];
  const tempValues: number[] = [];
  const pidValues: PidSample[] = [];

  points.forEach((point) => {
    if (!Array.isArray(point) || point.length < 7) {
      return;
    }
    tempValues.push(Number(point[1]));
    pidValues.push({
      target_c: Number(point[2]),
      kp: 14.0,
      ki: 0.35,
      kd: 6.0,
      output_percent: Number(point[3]),
      window_step: Number(point[4]),
      on_steps: Number(point[5]),
      relay_on: Number(point[6]),
    });
    lastHistorySeq = Number(point[0]);
  });

  const sampleIntervalS =
    Number.isFinite(Number(payload.sample_interval_s)) && Number(payload.sample_interval_s) > 0
      ? Number(payload.sample_interval_s)
      : TREND_SAMPLE_INTERVAL_SECONDS;
  loadedHistoryBaseSeconds = Math.max(0, tempValues.length - 1) * sampleIntervalS;

  sparkline.setValues(tempValues);
  pidChart.setValues(pidValues);
  sparkline.setElapsedSeconds(loadedHistoryBaseSeconds);
  pidChart.setElapsedSeconds(loadedHistoryBaseSeconds);
};

const mergeHistoryFromDevice = async (sparkline: Sparkline, pidChart: PidChart): Promise<void> => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json()) as HistoryPayload;
  const points = Array.isArray(payload.points) ? payload.points : [];

  points.forEach((point) => {
    if (!Array.isArray(point) || point.length < 7) {
      return;
    }
    const seq = Number(point[0]);
    if (!Number.isFinite(seq) || seq <= lastHistorySeq) {
      return;
    }
    lastHistorySeq = seq;
    sparkline.push(Number(point[1]));
    pidChart.push({
      target_c: Number(point[2]),
      kp: 14.0,
      ki: 0.35,
      kd: 6.0,
      output_percent: Number(point[3]),
      window_step: Number(point[4]),
      on_steps: Number(point[5]),
      relay_on: Number(point[6]),
    });
  });
};

const updateNtpPill = (synced: boolean): void => {
  const pill = byId<HTMLElement>("ntp-pill");
  if (synced) {
    pill.className = "status-pill status-ok";
    pill.textContent = "NTP synced";
  } else {
    pill.className = "status-pill status-warn";
    pill.textContent = "NTP pending";
  }
};

const updateFromStatus = (data: StatusPayload, sparkline: Sparkline, pidChart: PidChart): void => {
  if (typeof data.system.collecting === "boolean") {
    if (syncCollectingUi) {
      syncCollectingUi(data.system.collecting);
    } else {
      collecting = data.system.collecting;
    }
  }

  lastUptimeSeconds = data.system.uptime_s;
  if (collecting) {
    const totalElapsed = loadedHistoryBaseSeconds + data.system.uptime_s;
    sparkline.setElapsedSeconds(totalElapsed);
    pidChart.setElapsedSeconds(totalElapsed);
  }

  setText("title", `${data.device.toUpperCase()} CONTROL PANEL`);
  setText("updated", `Updated ${new Date().toLocaleTimeString()}`);

  setText("temp", formatTemp(data.sensor.ds18b20.temperature_c, "C"));
  setText("temp-secondary", formatTemp(data.sensor.ds18b20.temperature_f, "F"));

  if (collecting && data.sensor.ds18b20.temperature_c !== null) {
    sparkline.push(data.sensor.ds18b20.temperature_c);
  }

  setText("target", `${data.pid.target_c.toFixed(1)} C`);
  setText("target-secondary", `${data.pid.target_f.toFixed(1)} F`);
  const targetInput = byId<HTMLInputElement>("target-input");
  if (document.activeElement !== targetInput) {
    targetInput.value = data.pid.target_c.toFixed(1);
  }

  setText("pid", collecting ? `${data.pid.output_percent.toFixed(1)}%` : "0.0%");
  const relayState = !collecting ? "Deactivated" : (data.pid.relay_on ? "On" : "Off");
  setText("relay", relayState);
  byId<HTMLElement>("relay").style.color = relayState === "Deactivated" ? "#ff6e6e" : "";
  if (collecting) {
    pidChart.push({
    target_c: data.pid.target_c,
    kp: data.pid.kp,
    ki: data.pid.ki,
    kd: data.pid.kd,
    output_percent: data.pid.output_percent,
    window_step: data.pid.window_step,
    on_steps: data.pid.on_steps,
    relay_on: data.pid.relay_on ? 1 : 0,
    });
  }

  setText("ip", data.system.ip || "--");
  updateNtpPill(data.system.ntp.synced);

  setText("probe", data.sensor.ds18b20.name || "--");
  setText("sensor-status", data.sensor.ds18b20.error || "none");
  setText("window-step", String(data.pid.window_step));
  setText("on-steps", String(data.pid.on_steps));
  setText("uptime", formatUptime(data.system.uptime_s));

};

const loop = async (sparkline: Sparkline, pidChart: PidChart): Promise<void> => {
  if (collectionToggleInFlight || pollRequestInFlight) {
    return;
  }

  pollRequestInFlight = true;
  try {
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = (await response.json()) as StatusPayload;
    updateFromStatus(payload, sparkline, pidChart);
    if (collecting) {
      await mergeHistoryFromDevice(sparkline, pidChart);
    }
  } catch (error) {
    setText("updated", `Update failed: ${String(error)}`);
    const pill = byId<HTMLElement>("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  } finally {
    pollRequestInFlight = false;
  }
};

const start = (): void => {
  const chart = byId<HTMLCanvasElement>("temp-chart");
  const pidCanvas = byId<HTMLCanvasElement>("pid-chart");
  const sparkline = new Sparkline(chart);
  const pidChart = new PidChart(pidCanvas);
  const hoverRatioForClientX = (canvas: HTMLCanvasElement, clientX: number): number => {
    const rect = canvas.getBoundingClientRect();
    const canvasX = ((clientX - rect.left) / rect.width) * canvas.width;
    const axisPadLeft = 46;
    const plotWidth = Math.max(1, canvas.width - axisPadLeft - 6);
    return Math.max(0, Math.min(1, (canvasX - axisPadLeft) / plotWidth));
  };

  chart.addEventListener("mousemove", (event: MouseEvent) => {
    pidChart.setHoverRatio(hoverRatioForClientX(chart, event.clientX));
  });
  chart.addEventListener("mouseleave", () => {
    pidChart.setHoverRatio(null);
  });
  pidCanvas.addEventListener("mousemove", (event: MouseEvent) => {
    sparkline.setHoverRatio(hoverRatioForClientX(pidCanvas, event.clientX));
  });
  pidCanvas.addEventListener("mouseleave", () => {
    sparkline.setHoverRatio(null);
  });

  const applyZoom = (pivotRatio: number, factor: number): void => {
    const span = zoomEnd - zoomStart;
    const newSpan = Math.max(0.02, Math.min(1, span * factor));
    const center = zoomStart + pivotRatio * span;
    zoomStart = Math.max(0, center - pivotRatio * newSpan);
    zoomEnd = zoomStart + newSpan;
    if (zoomEnd > 1) { zoomEnd = 1; zoomStart = Math.max(0, 1 - newSpan); }
    sparkline.redraw();
    pidChart.redraw();
  };

  const applyPan = (delta: number): void => {
    const span = zoomEnd - zoomStart;
    const newStart = Math.max(0, Math.min(1 - span, zoomStart + delta * span));
    zoomStart = newStart;
    zoomEnd = newStart + span;
    sparkline.redraw();
    pidChart.redraw();
  };

  const onWheelZoom = (canvas: HTMLCanvasElement, e: WheelEvent): void => {
    e.preventDefault();
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY) * 0.3) {
      applyPan(e.deltaX / 800);
    } else if (e.deltaY !== 0) {
      const ratio = hoverRatioForClientX(canvas, e.clientX);
      applyZoom(ratio, e.deltaY > 0 ? 1.25 : 0.8);
    }
  };

  const resetZoom = (): void => {
    zoomStart = 0;
    zoomEnd = 1;
    sparkline.redraw();
    pidChart.redraw();
  };

  chart.addEventListener("wheel", (e: WheelEvent) => onWheelZoom(chart, e), { passive: false });
  pidCanvas.addEventListener("wheel", (e: WheelEvent) => onWheelZoom(pidCanvas, e), { passive: false });
  chart.addEventListener("dblclick", resetZoom);
  pidCanvas.addEventListener("dblclick", resetZoom);

  const targetInput = byId<HTMLInputElement>("target-input");
  const targetSubmit = byId<HTMLButtonElement>("target-submit");

  const applyTarget = async (): Promise<void> => {
    const parsed = Number.parseFloat(targetInput.value);
    if (!Number.isFinite(parsed)) {
      setTargetFeedback("Enter a valid number", "error");
      return;
    }
    if (parsed < -20 || parsed > 25) {
      setTargetFeedback("Target must be between -20 and 25 C", "error");
      return;
    }

    targetSubmit.disabled = true;
    setTargetFeedback("Applying target...");
    try {
      await submitTargetTemperature(parsed);
      setTargetFeedback(`Applied ${parsed.toFixed(1)} C`, "ok");
      await loop(sparkline, pidChart);
    } catch (error) {
      setTargetFeedback(`Apply failed: ${String(error)}`, "error");
    } finally {
      targetSubmit.disabled = false;
    }
  };

  targetSubmit.addEventListener("click", () => {
    void applyTarget();
  });
  targetInput.addEventListener("keydown", (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void applyTarget();
    }
  });

  loadHistoryFromDevice(sparkline, pidChart)
    .catch((error) => {
      setText("updated", `History load failed: ${String(error)}`);
    })
    .finally(() => {
      void loop(sparkline, pidChart);
    });
  window.setInterval(() => {
    void loop(sparkline, pidChart);
  }, 2000);

  const menuBtn = byId<HTMLButtonElement>("menu-btn");
  const menuDropdown = byId<HTMLElement>("menu-dropdown");
  const clearDataBtn = byId<HTMLButtonElement>("clear-data");
  const startDataBtn = byId<HTMLButtonElement>("start-data");
  const stopDataBtn = byId<HTMLButtonElement>("stop-data");

  const setCollecting = (value: boolean): void => {
    collecting = value;
    startDataBtn.disabled = value;
    stopDataBtn.disabled = !value;
  };

  syncCollectingUi = setCollecting;

  setCollecting(false);

  startDataBtn.addEventListener("click", () => {
    if (collectionToggleInFlight) {
      return;
    }
    collectionToggleInFlight = true;
    startDataBtn.disabled = true;
    stopDataBtn.disabled = true;
    void setCollectionOnDevice(true)
      .then(() => {
        setCollecting(true);
      })
      .catch((error) => {
        setText("updated", `Start failed: ${String(error)}`);
        setCollecting(false);
      })
      .finally(() => {
        collectionToggleInFlight = false;
      });
  });

  stopDataBtn.addEventListener("click", () => {
    if (collectionToggleInFlight) {
      return;
    }
    collectionToggleInFlight = true;
    startDataBtn.disabled = true;
    stopDataBtn.disabled = true;
    void setCollectionOnDevice(false)
      .then(() => {
        setCollecting(false);
      })
      .catch((error) => {
        setText("updated", `Stop failed: ${String(error)}`);
        setCollecting(true);
      })
      .finally(() => {
        collectionToggleInFlight = false;
      });
  });

  menuBtn.addEventListener("click", (event: MouseEvent) => {
    event.stopPropagation();
    const isOpen = menuDropdown.classList.toggle("open");
    menuBtn.setAttribute("aria-expanded", String(isOpen));
  });

  menuDropdown.addEventListener("click", (event: MouseEvent) => {
    event.stopPropagation();
  });

  document.addEventListener("click", () => {
    menuDropdown.classList.remove("open");
    menuBtn.setAttribute("aria-expanded", "false");
  });

  clearDataBtn.addEventListener("click", () => {
    clearHistoryOnDevice()
      .then(() => {
        sparkline.clear();
        pidChart.clear();
        lastUptimeSeconds = null;
      })
      .catch((error) => {
        setText("updated", `Clear failed: ${String(error)}`);
      })
      .finally(() => {
        menuDropdown.classList.remove("open");
        menuBtn.setAttribute("aria-expanded", "false");
      });
  });
};

start();
