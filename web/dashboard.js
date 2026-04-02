const TREND_STORAGE_KEY = "brewster.dashboard.tempTrend.v1";

class Sparkline {
  constructor(canvas, maxPoints = 120) {
    this.canvas = canvas;
    this.values = [];
    this.maxPoints = maxPoints;
  }

  push(value) {
    this.values.push(value);
    if (this.values.length > this.maxPoints) {
      this.values.shift();
    }
    this.draw();
  }

  replaceValues(values) {
    this.values.length = 0;
    values.slice(-this.maxPoints).forEach((value) => {
      this.values.push(value);
    });
    this.draw();
  }

  snapshot() {
    return [...this.values];
  }

  draw() {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this.values.length < 2) return;

    const min = Math.min(...this.values);
    const max = Math.max(...this.values);
    const spread = Math.max(0.1, max - min);
    const xStep = width / (this.values.length - 1);

    const yFor = (v) => {
      const norm = (v - min) / spread;
      return height - 8 - norm * (height - 16);
    };

    const gradient = ctx.createLinearGradient(0, 0, width, 0);
    gradient.addColorStop(0, "#40c4ff");
    gradient.addColorStop(1, "#40d990");

    ctx.lineWidth = 2;
    ctx.strokeStyle = gradient;
    ctx.beginPath();

    this.values.forEach((v, i) => {
      const x = i * xStep;
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

const byId = (id) => {
  const el = document.getElementById(id);
  if (!el) {
    throw new Error(`Missing element: ${id}`);
  }
  return el;
};

const setText = (id, text) => {
  byId(id).textContent = text;
};

const formatTemp = (value, unit) => {
  if (value === null || Number.isNaN(value)) return `--.- ${unit}`;
  return `${value.toFixed(1)} ${unit}`;
};

const formatNumber = (value, suffix = "") => {
  if (value === null || Number.isNaN(value)) return "--";
  return `${value.toFixed(2)}${suffix}`;
};

const formatUptime = (uptimeSec) => {
  const h = Math.floor(uptimeSec / 3600);
  const m = Math.floor((uptimeSec % 3600) / 60);
  const s = uptimeSec % 60;
  return `${h}h ${m}m ${s}s`;
};

const loadPersistedTrend = () => {
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
      .filter((value) => typeof value === "number" && Number.isFinite(value))
      .slice(-240);
  } catch {
    return [];
  }
};

const persistTrend = (values) => {
  try {
    window.localStorage.setItem(TREND_STORAGE_KEY, JSON.stringify(values.slice(-240)));
  } catch {
    // Ignore storage errors (quota, privacy mode, etc.).
  }
};

const updateNtpPill = (synced) => {
  const pill = byId("ntp-pill");
  if (synced) {
    pill.className = "status-pill status-ok";
    pill.textContent = "NTP synced";
  } else {
    pill.className = "status-pill status-warn";
    pill.textContent = "NTP pending";
  }
};

const updateFromStatus = (data, sparkline) => {
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

const loop = async (sparkline) => {
  try {
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = await response.json();
    updateFromStatus(payload, sparkline);
  } catch (error) {
    setText("updated", `Update failed: ${String(error)}`);
    const pill = byId("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  }
};

const start = () => {
  const chart = byId("temp-chart");
  const sparkline = new Sparkline(chart);
  sparkline.replaceValues(loadPersistedTrend());

  void loop(sparkline);
  window.setInterval(() => {
    void loop(sparkline);
  }, 2000);
};

start();
