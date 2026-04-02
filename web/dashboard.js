const TREND_SAMPLE_INTERVAL_SECONDS = 2;
const HISTORY_FETCH_POINTS = 2000;
let lastHistorySeq = -1;
let collecting = false;
let syncCollectingUi = null;
let collectionToggleInFlight = false;
let pollRequestInFlight = false;
const NO_DATA_FONT = "700 20px 'Avenir Next', 'Trebuchet MS', sans-serif";

const delayMs = (ms) => new Promise((resolve) => {
  window.setTimeout(resolve, ms);
});

const drawNoData = (ctx, width, height) => {
  ctx.save();
  ctx.font = NO_DATA_FONT;
  ctx.fillStyle = "rgba(230, 241, 255, 0.72)";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("No Data", Math.round(width / 2), Math.round(height / 2));
  ctx.restore();
};

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
    this.hoverX = null;
    this.elapsedSeconds = null;
    this.canvas.addEventListener("mousemove", (event) => {
      this.updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this.hoverX = null;
      this.draw();
    });
  }

  setValues(values) {
    this.values.length = 0;
    this.values.push(...values);
    this.draw();
  }

  setElapsedSeconds(seconds) {
    this.elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this.draw();
  }

  updateHover(clientX) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
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

    if (this.values.length < 2) {
      drawNoData(ctx, width, height);
      return;
    }

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
    const elapsedSeconds = this.elapsedSeconds ?? ((this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
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

    if (this.hoverX !== null && this.values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this.hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const index = Math.round(ratio * (this.values.length - 1));
      const value = this.values[index];
      const x = clampedX;
      const y = yFor(value);
      const tip = `${value.toFixed(2)} C  T+${formatElapsed(Math.round(ratio * elapsedSeconds))}`;

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
  }
}

class PidChart {
  constructor(canvas) {
    this.canvas = canvas;
    this.values = [];
    this.hoverX = null;
    this.elapsedSeconds = null;
    this.canvas.addEventListener("mousemove", (event) => {
      this.updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this.hoverX = null;
      this.draw();
    });
  }

  setValues(values) {
    this.values.length = 0;
    this.values.push(...values);
    this.draw();
  }

  setElapsedSeconds(seconds) {
    this.elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this.draw();
  }

  updateHover(clientX) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this.hoverX !== x) {
      this.hoverX = x;
      this.draw();
    }
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
    const elapsedSeconds = this.elapsedSeconds ?? ((this.values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
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

    if (this.hoverX !== null && this.values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this.hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const i = Math.round(ratio * (this.values.length - 1));
      const sample = this.values[i];
      const x = clampedX;
      const tip1 = `T+${formatElapsed(Math.round(ratio * elapsedSeconds))}`;
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

const clearHistoryOnDevice = async () => {
  const response = await fetch("/history/clear", { method: "POST" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
};

const setCollectionOnDevice = async (enabled) => {
  const path = enabled ? "/collection/start" : "/collection/stop";
  // Avoid colliding control requests with in-flight polling on the single-connection HTTP task.
  for (let i = 0; i < 6 && pollRequestInFlight; i += 1) {
    await delayMs(80);
  }
  let lastError = null;
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

const loadHistoryFromDevice = async (sparkline, pidChart) => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = await response.json();
  const points = Array.isArray(payload.points) ? payload.points : [];
  const tempValues = [];
  const pidValues = [];

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

  sparkline.setValues(tempValues);
  pidChart.setValues(pidValues);
};

const mergeHistoryFromDevice = async (sparkline, pidChart) => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = await response.json();
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
  if (typeof data.system.collecting === "boolean") {
    if (syncCollectingUi) {
      syncCollectingUi(data.system.collecting);
    } else {
      collecting = data.system.collecting;
    }
  }

  lastUptimeSeconds = data.system.uptime_s;
  if (collecting) {
    sparkline.setElapsedSeconds(data.system.uptime_s);
    pidChart.setElapsedSeconds(data.system.uptime_s);
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
  const targetInput = byId("target-input");
  if (document.activeElement !== targetInput) {
    targetInput.value = data.pid.target_c.toFixed(1);
  }

  setText("pid", collecting ? `${data.pid.output_percent.toFixed(1)}%` : "0.0%");
  const relayState = !collecting ? "Deactivated" : (data.pid.relay_on ? "On" : "Off");
  setText("relay", relayState);
  byId("relay").style.color = relayState === "Deactivated" ? "#ff6e6e" : "";
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

const loop = async (sparkline, pidChart) => {
  if (collectionToggleInFlight || pollRequestInFlight) {
    return;
  }

  pollRequestInFlight = true;
  try {
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = await response.json();
    updateFromStatus(payload, sparkline, pidChart);
    if (collecting) {
      await mergeHistoryFromDevice(sparkline, pidChart);
    }
  } catch (error) {
    setText("updated", `Update failed: ${String(error)}`);
    const pill = byId("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  } finally {
    pollRequestInFlight = false;
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
  targetInput.addEventListener("keydown", (event) => {
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

  const menuBtn = byId("menu-btn");
  const menuDropdown = byId("menu-dropdown");
  const clearDataBtn = byId("clear-data");
  const startDataBtn = byId("start-data");
  const stopDataBtn = byId("stop-data");

  const setCollecting = (value) => {
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

  menuBtn.addEventListener("click", (event) => {
    event.stopPropagation();
    const isOpen = menuDropdown.classList.toggle("open");
    menuBtn.setAttribute("aria-expanded", String(isOpen));
  });

  menuDropdown.addEventListener("click", (event) => {
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
