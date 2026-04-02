export {};

type NullableNumber = number | null;

const TREND_STORAGE_KEY = "brewster.dashboard.tempTrend.v1";
const TREND_SAMPLE_INTERVAL_SECONDS = 2;

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

class Sparkline {
  private readonly canvas: HTMLCanvasElement;
  private readonly values: number[] = [];

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
  }

  push(value: number): void {
    this.values.push(value);
    this.draw();
  }

  replaceValues(values: number[]): void {
    this.values.length = 0;
    values.forEach((value) => {
      this.values.push(value);
    });
    this.draw();
  }

  snapshot(): number[] {
    return [...this.values];
  }

  private draw(): void {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this.values.length < 2) return;

    const min = Math.min(...this.values);
    const max = Math.max(...this.values);
    const spread = Math.max(0.1, max - min);
    const axisPadLeft = 46;
    const plotPadTop = 8;
    const plotPadBottom = 8;
    const plotWidth = Math.max(1, width - axisPadLeft - 6);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);
    const xStep = this.values.length > 1 ? plotWidth / (this.values.length - 1) : 0;

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
    const elapsedSeconds = (this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS;
    ctx.save();
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    ctx.fillText(`T+${formatElapsed(elapsedSeconds)}`, width - 4, height - 2);
    ctx.restore();

    const gradient = ctx.createLinearGradient(0, 0, width, 0);
    gradient.addColorStop(0, "#40c4ff");
    gradient.addColorStop(1, "#40d990");

    ctx.lineWidth = 2;
    ctx.strokeStyle = gradient;
    ctx.beginPath();

    this.values.forEach((v, i) => {
      const x = axisPadLeft + i * xStep;
      const y = yFor(v);
      if (i === 0) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, y);
      }
    });

    ctx.stroke();
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

const loadPersistedTrend = (): number[] => {
  try {
    const raw = window.localStorage.getItem(TREND_STORAGE_KEY);
    if (!raw) {
      return [];
    }
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed
      .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  } catch {
    return [];
  }
};

const persistTrend = (values: number[]): void => {
  try {
    window.localStorage.setItem(TREND_STORAGE_KEY, JSON.stringify(values));
  } catch {
    // Ignore storage errors (quota, privacy mode, etc.).
  }
};

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

const updateFromStatus = (data: StatusPayload, sparkline: Sparkline): void => {
  setText("title", `${data.device.toUpperCase()} CONTROL PANEL`);
  setText("updated", `Updated ${new Date().toLocaleTimeString()}`);

  setText("temp", formatTemp(data.sensor.ds18b20.temperature_c, "C"));
  setText("temp-secondary", formatTemp(data.sensor.ds18b20.temperature_f, "F"));

  if (data.sensor.ds18b20.temperature_c !== null) {
    sparkline.push(data.sensor.ds18b20.temperature_c);
    persistTrend(sparkline.snapshot());
  }

  setText("target", `${data.pid.target_c.toFixed(1)} C`);
  setText("target-secondary", `${data.pid.target_f.toFixed(1)} F`);
  const targetInput = byId<HTMLInputElement>("target-input");
  if (document.activeElement !== targetInput) {
    targetInput.value = data.pid.target_c.toFixed(1);
  }

  setText("pid", `${data.pid.output_percent.toFixed(1)}%`);
  setText("relay", data.pid.relay_on ? "Relay ON" : "Relay OFF");

  setText("ip", data.system.ip || "--");
  updateNtpPill(data.system.ntp.synced);

  setText("probe", data.sensor.ds18b20.name || "--");
  setText("sensor-status", data.sensor.ds18b20.error || "none");
  setText("window-step", String(data.pid.window_step));
  setText("on-steps", String(data.pid.on_steps));
  setText("uptime", formatUptime(data.system.uptime_s));

  setText("ntp-source", data.system.ntp.master_source ?? "--");
  setText("ntp-address", data.system.ntp.master_address ?? "--");
  setText(
    "ntp-offset",
    `${formatNumber(data.system.ntp.master_offset_ms, " ms")} / ${formatNumber(data.system.ntp.master_offset_jitter_ms, " ms")}`,
  );
  setText(
    "ntp-latency",
    `${formatNumber(data.system.ntp.master_latency_ms, " ms")} / ${formatNumber(data.system.ntp.master_jitter_ms, " ms")}`,
  );
  setText("ntp-time", data.system.ntp.time ?? "--");
};

const loop = async (sparkline: Sparkline): Promise<void> => {
  try {
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = (await response.json()) as StatusPayload;
    updateFromStatus(payload, sparkline);
  } catch (error) {
    setText("updated", `Update failed: ${String(error)}`);
    const pill = byId<HTMLElement>("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  }
};

const start = (): void => {
  const chart = byId<HTMLCanvasElement>("temp-chart");
  const sparkline = new Sparkline(chart);
  sparkline.replaceValues(loadPersistedTrend());
  const targetInput = byId<HTMLInputElement>("target-input");
  const targetSubmit = byId<HTMLButtonElement>("target-submit");

  const applyTarget = async (): Promise<void> => {
    const parsed = Number.parseFloat(targetInput.value);
    if (!Number.isFinite(parsed)) {
      setTargetFeedback("Enter a valid number", "error");
      return;
    }
    if (parsed < 25 || parsed > 150) {
      setTargetFeedback("Target must be between 25 and 150 C", "error");
      return;
    }

    targetSubmit.disabled = true;
    setTargetFeedback("Applying target...");
    try {
      await submitTargetTemperature(parsed);
      setTargetFeedback(`Applied ${parsed.toFixed(1)} C`, "ok");
      await loop(sparkline);
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

  void loop(sparkline);
  window.setInterval(() => {
    void loop(sparkline);
  }, 2000);
};

start();
