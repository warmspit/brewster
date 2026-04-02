const TREND_SAMPLE_INTERVAL_SECONDS = 2;

const formatElapsed = (totalSeconds) => {
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

class Sparkline {
  constructor(canvas) {
    this.canvas = canvas;
    this.values = [];
  }

  push(value) {
    this.values.push(value);
    this.draw();
  }

  clear() {
    this.values.length = 0;
    this.draw();
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
    const axisPadLeft = 46;
    const plotPadTop = 8;
    const plotPadBottom = 8;
    const plotWidth = Math.max(1, width - axisPadLeft - 6);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);
    const xStep = this.values.length > 1 ? plotWidth / (this.values.length - 1) : 0;

    const yFor = (v) => {
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
      ctx.strokeStyle = axisColor;
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - 4, y);
      ctx.stroke();
      ctx.fillText(`${tickValue.toFixed(1)} C`, 2, y + 4);
    });

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

class PidChart {
  constructor(canvas) {
    this.canvas = canvas;
    this.values = [];
  }

  push(sample) {
    this.values.push(sample);
    this.draw();
  }

  clear() {
    this.values.length = 0;
    this.draw();
  }

  draw() {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this.values.length < 2) return;

    const axisPadLeft = 46;
    const plotPadTop = 8;
    const plotPadBottom = 8;
    const plotWidth = Math.max(1, width - axisPadLeft - 6);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);

    const series = [
      { color: "#f7d774", value: (p) => p.target_c },
      { color: "#6ec5ff", value: (p) => p.kp },
      { color: "#8ef0c8", value: (p) => p.ki },
      { color: "#b28cff", value: (p) => p.kd },
      { color: "#ff8d6e", value: (p) => p.output_percent },
      { color: "#7cf3ff", value: (p) => p.window_step },
      { color: "#ffb3d1", value: (p) => p.on_steps },
      { color: "#ffffff", value: (p) => p.relay_on },
    ];

    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    this.values.forEach((point) => {
      series.forEach((entry) => {
        const v = entry.value(point);
        if (v < min) min = v;
        if (v > max) max = v;
      });
    });
    const spread = Math.max(0.1, max - min);
    const xStep = plotWidth / (this.values.length - 1);
    const yFor = (v) => {
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
    const elapsedSeconds = (this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS;
    ctx.save();
    ctx.textAlign = "right";
    ctx.fillText(`T+${formatElapsed(elapsedSeconds)}`, width - 4, height - 2);
    ctx.restore();

    series.forEach((entry, idx) => {
      ctx.beginPath();
      ctx.lineWidth = idx === series.length - 1 ? 1.2 : 1.8;
      ctx.strokeStyle = entry.color;
      this.values.forEach((point, i) => {
        const x = axisPadLeft + i * xStep;
        const y = yFor(entry.value(point));
        if (i === 0) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      });
      ctx.stroke();
    });
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

let lastUptimeSeconds = null;

const setTargetFeedback = (text, tone = "normal") => {
  const feedback = byId("target-feedback");
  feedback.textContent = text;
  if (tone === "ok") {
    feedback.style.color = "#40d990";
  } else if (tone === "error") {
    feedback.style.color = "#ff6e6e";
  } else {
    feedback.style.color = "";
  }
};

const submitTargetTemperature = async (tempC) => {
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

const updateFromStatus = (data, sparkline, pidChart) => {
  if (lastUptimeSeconds !== null && data.system.uptime_s < lastUptimeSeconds) {
    sparkline.clear();
    pidChart.clear();
  }
  lastUptimeSeconds = data.system.uptime_s;

  setText("title", `${data.device.toUpperCase()} CONTROL PANEL`);
  setText("updated", `Updated ${new Date().toLocaleTimeString()}`);

  setText("temp", formatTemp(data.sensor.ds18b20.temperature_c, "C"));
  setText("temp-secondary", formatTemp(data.sensor.ds18b20.temperature_f, "F"));

  if (data.sensor.ds18b20.temperature_c !== null) {
    sparkline.push(data.sensor.ds18b20.temperature_c);
  }

  setText("target", `${data.pid.target_c.toFixed(1)} C`);
  setText("target-secondary", `${data.pid.target_f.toFixed(1)} F`);
  const targetInput = byId("target-input");
  if (document.activeElement !== targetInput) {
    targetInput.value = data.pid.target_c.toFixed(1);
  }

  setText("pid", `${data.pid.output_percent.toFixed(1)}%`);
  setText("relay", data.pid.relay_on ? "Relay ON" : "Relay OFF");
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

  setText("ip", data.system.ip || "--");
  updateNtpPill(data.system.ntp.synced);

  setText("probe", data.sensor.ds18b20.name || "--");
  setText("sensor-status", data.sensor.ds18b20.error || "none");
  setText("window-step", String(data.pid.window_step));
  setText("on-steps", String(data.pid.on_steps));
  setText("uptime", formatUptime(data.system.uptime_s));

};

const loop = async (sparkline, pidChart) => {
  try {
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = await response.json();
    updateFromStatus(payload, sparkline, pidChart);
  } catch (error) {
    setText("updated", `Update failed: ${String(error)}`);
    const pill = byId("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  }
};

const start = () => {
  const chart = byId("temp-chart");
  const pidCanvas = byId("pid-chart");
  const sparkline = new Sparkline(chart);
  const pidChart = new PidChart(pidCanvas);
  const targetInput = byId("target-input");
  const targetSubmit = byId("target-submit");

  const applyTarget = async () => {
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
  targetInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void applyTarget();
    }
  });

  void loop(sparkline, pidChart);
  window.setInterval(() => {
    void loop(sparkline, pidChart);
  }, 2000);
};

start();
