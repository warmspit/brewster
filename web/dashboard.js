// --- ui.js ---
const delayMs = (ms) => new Promise((resolve) => {
  window.setTimeout(resolve, ms);
});

const byId = (id) => {
  const el = document.getElementById(id);
  if (!el) {
    throw new Error(`Missing element: ${id}`);
  }
  return el;
};

const setText = (id, text) => {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
};

const formatTemp = (value, unit) => {
  if (value === null || Number.isNaN(value)) return `--.- ${unit}`;
  return `${value.toFixed(1)} ${unit}`;
};

const formatNumber = (value, suffix = "") => {
  if (value === null || Number.isNaN(value)) return "--";
  return `${value.toFixed(2)}${suffix}`;
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

const formatUptime = (uptimeSec) => {
  const h = Math.floor(uptimeSec / 3600);
  const m = Math.floor((uptimeSec % 3600) / 60);
  const s = uptimeSec % 60;
  return `${h}h ${m}m ${s}s`;
};

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

// --- api.js ---
const HISTORY_LOAD_POINTS  = 10000; // full replay on page load — fetch as many records as the server holds
const HISTORY_MERGE_POINTS =   120; // only need last ~2 min on each 1-second poll tick
/// Width of the auto-follow live view. When total data exceeds this duration,
/// the chart auto-scrolls so the newest data is always at the right edge.
const LIVE_WINDOW_SECONDS = 3600; // 1 hour

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

const setCollectionOnDevice = async (enabled, getPollInFlight) => {
  const path = enabled ? "/collection/start" : "/collection/stop";
  // Avoid colliding control requests with in-flight polling on the single-connection HTTP task.
  for (let i = 0; i < 6 && getPollInFlight(); i += 1) {
    await new Promise((resolve) => { window.setTimeout(resolve, 80); });
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
        await new Promise((resolve) => { window.setTimeout(resolve, 120 * attempt); });
      }
    }
  }
  throw lastError instanceof Error ? lastError : new Error(String(lastError));
};

// --- charts.js ---
const TREND_SAMPLE_INTERVAL_SECONDS = 2;
const CHART_CANVAS_WIDTH = 1120;
const CHART_CANVAS_HEIGHT = 220;
const NO_DATA_FONT = "700 20px 'Avenir Next', 'Trebuchet MS', sans-serif";

const CHART_LAYOUT = {
  axisPadLeft: 58,
  plotPadTop: 8,
  plotPadBottom: 8,
  sparklinePadRight: 6,
  pidPadRight: 24,
};

// Zoom window state: owned here so chart drawNow() can read without coupling to app module.
let zoomStart = 0;
let zoomEnd = 1;

const setZoomWindow = (start, end) => {
  zoomStart = start;
  zoomEnd = end;
};

const drawNoData = (ctx, width, height) => {
  const { axisPadLeft, plotPadTop, plotPadBottom } = CHART_LAYOUT;
  const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);
  const axisColor = "rgba(159, 180, 203, 0.35)";

  ctx.strokeStyle = axisColor;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(axisPadLeft, plotPadTop);
  ctx.lineTo(axisPadLeft, height - plotPadBottom);
  ctx.stroke();

  ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
  ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
  ctx.textAlign = "right";

  const tickValues = [100, 50, 0];
  tickValues.forEach((tickValue) => {
    const norm = tickValue / 100;
    const y = height - plotPadBottom - norm * plotHeight;
    ctx.strokeStyle = axisColor;
    ctx.beginPath();
    ctx.moveTo(axisPadLeft, y);
    ctx.lineTo(width - 4, y);
    ctx.stroke();
    ctx.fillText(`${tickValue.toFixed(0)}°C`, axisPadLeft - 4, y + 4);
  });

  ctx.strokeStyle = axisColor;
  ctx.beginPath();
  ctx.moveTo(axisPadLeft, height - plotPadBottom);
  ctx.lineTo(width - 4, height - plotPadBottom);
  ctx.stroke();

  ctx.save();
  ctx.font = NO_DATA_FONT;
  ctx.fillStyle = "rgba(230, 241, 255, 0.72)";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("No Data", Math.round(width / 2), Math.round(height / 2));
  ctx.restore();
};

class Sparkline {
  constructor(canvas, { showTarget = true } = {}) {
    this.canvas = canvas;
    this._showTarget = showTarget;
    this._values = [];
    this._targetValues = [];
    this._gapBefore = new Set();
    this._pendingGap = false;
    this._hoverX = null;
    this._elapsedSeconds = null;
    this._rafId = null;
    this._deadband = 0;
    // Hover is driven externally via setHoverRatio() broadcast from the section-level listener.
  }

  setDeadband(deadband_c) {
    this._deadband = Number.isFinite(deadband_c) ? Math.max(0, deadband_c) : 0;
    this._draw();
  }

  setValues(values) {
    this._values.splice(0, this._values.length, ...values);
    this._draw();
  }

  setElapsedSeconds(seconds) {
    this._elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this._draw();
  }

  setGapBefore(indices) {
    this._gapBefore = new Set(indices);
    this._draw();
  }

  markGapBeforeNext() {
    this._pendingGap = true;
  }

  setHoverRatio(ratio) {
    if (ratio === null) {
      if (this._hoverX !== null) {
        this._hoverX = null;
        this._drawNow();
      }
      return;
    }
    const { axisPadLeft, sparklinePadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - sparklinePadRight);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._drawNow();
    }
  }

  _updateHover(clientX) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._draw();
    }
  }

  push(value) {
    if (this._pendingGap) {
      this._gapBefore.add(this._values.length);
      this._pendingGap = false;
    }
    this._values.push(value);
    this._draw();
  }

  setTargetValues(values) {
    this._targetValues = values.slice();
    this._draw();
  }

  pushTarget(value) {
    this._targetValues.push(value);
  }

  clear() {
    this._values.length = 0;
    this._targetValues.length = 0;
    this._draw();
  }

  redraw() {
    this._draw();
  }

  _draw() {
    if (this._rafId !== null) {
      return;
    }
    this._rafId = window.requestAnimationFrame(() => {
      this._rafId = null;
      this._drawNow();
    });
  }

  _drawNow() {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this._values.length < 2) {
      drawNoData(ctx, width, height);
      return;
    }

    const { axisPadLeft, plotPadTop, plotPadBottom, sparklinePadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, width - axisPadLeft - sparklinePadRight);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);
    const n = this._values.length;
    const visStart = zoomStart * (n - 1);
    const visEnd = zoomEnd * (n - 1);
    const iFirst = Math.max(0, Math.floor(visStart));
    const iLast = Math.min(n - 1, Math.ceil(visEnd));
    const xForIdx = (i) => axisPadLeft + ((i - visStart) / Math.max(1, visEnd - visStart)) * plotWidth;
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    for (let i = iFirst; i <= iLast; i++) {
      const v = this._values[i];
      if (v < min) min = v;
      if (v > max) max = v;
    }
    // Include the target temperature in the Y scale so its line always stays
    // within the plot area and doesn't cause the scale to behave erratically.
    if (this._showTarget) {
      for (let i = iFirst; i <= iLast; i++) {
        if (i >= this._targetValues.length) break;
        const v = this._targetValues[i];
        if (!Number.isFinite(v)) continue;
        if (v < min) min = v;
        if (v > max) max = v;
      }
    }
    // Add 1 °C buffer to top and bottom so the trace never touches the edge.
    min -= 1;
    max += 1;
    const spread = Math.max(0.1, max - min);

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
    const decimals = spread >= 10 ? 0 : spread >= 0.5 ? 1 : 2;
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    tickValues.forEach((tickValue) => {
      const label = `${tickValue.toFixed(decimals)}°C`;
      const y = yFor(tickValue);
      ctx.strokeStyle = axisColor;
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - 4, y);
      ctx.stroke();
      const textY = Math.max(14, Math.min(y + 4, height - 4));
      ctx.fillText(label, axisPadLeft - 4, textY);
    });
    ctx.textAlign = "left";

    ctx.beginPath();
    ctx.moveTo(axisPadLeft, height - plotPadBottom);
    ctx.lineTo(width - 4, height - plotPadBottom);
    ctx.stroke();
    const elapsedSeconds = this._elapsedSeconds ?? ((this._values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
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

    // Smooth out DS18B20 quantization noise (±1 LSB = ±0.0625°C at 1Hz)
    // with a small moving-average window. Window shrinks near gap boundaries
    // so gaps don't bleed across segments.
    const SMOOTH_HALF = 2; // ±2 samples → 5-sample window
    const smoothed = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      let sum = 0, count = 0;
      for (let k = i - SMOOTH_HALF; k <= i + SMOOTH_HALF; k++) {
        if (k < 0 || k >= n) continue;
        // Don't average across a gap boundary.
        if (k > i && this._gapBefore.has(k)) break;
        if (k < i) {
          let crossesGap = false;
          for (let g = k + 1; g <= i; g++) { if (this._gapBefore.has(g)) { crossesGap = true; break; } }
          if (crossesGap) continue;
        }
        sum += this._values[k];
        count++;
      }
      smoothed[i] = count > 0 ? sum / count : this._values[i];
    }

    ctx.save();
    ctx.beginPath();
    ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
    ctx.clip();
    ctx.lineWidth = 2;
    ctx.strokeStyle = gradient;
    ctx.beginPath();
    for (let i = iFirst; i <= iLast; i++) {
      const x = xForIdx(i);
      const y = yFor(smoothed[i]);
      if (i === iFirst || this._gapBefore.has(i)) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, y);
      }
    }
    ctx.stroke();

    // Draw red vertical marks at gap positions.
    if (this._gapBefore.size > 0) {
      ctx.strokeStyle = "#ff4444";
      ctx.lineWidth = 2;
      for (const gi of this._gapBefore) {
        if (gi < iFirst || gi > iLast) continue;
        const x = xForIdx(gi);
        ctx.beginPath();
        ctx.moveTo(x, plotPadTop);
        ctx.lineTo(x, height - plotPadBottom);
        ctx.stroke();
      }
    }
    ctx.restore();

    if (this._targetValues.length > 1 && this._showTarget) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
      ctx.clip();

      // ── Deadband shaded band ──────────────────────────────────────────────
      // Shade ±(deadband/2) around the target line as a translucent yellow band.
      if (this._deadband > 0) {
        const half = this._deadband / 2;
        ctx.fillStyle = "rgba(247, 215, 116, 0.10)";
        let bandStarted = false;
        let bx0 = 0;
        let prevV = null;
        const flushBand = (x1, v) => {
          if (!bandStarted || prevV === null) return;
          const yTop = yFor(prevV + half);
          const yBot = yFor(prevV - half);
          ctx.fillRect(bx0, Math.min(yTop, yBot), x1 - bx0, Math.abs(yBot - yTop));
        };
        for (let i = iFirst; i <= iLast; i++) {
          if (i >= this._targetValues.length) break;
          const v = this._targetValues[i];
          const x = xForIdx(i);
          if (!Number.isFinite(v)) { flushBand(x, prevV); bandStarted = false; prevV = null; continue; }
          const jumped = prevV !== null && Math.abs(v - prevV) > 0.05;
          if (!bandStarted || jumped) { flushBand(x, prevV); bx0 = x; bandStarted = true; }
          prevV = v;
        }
        if (bandStarted && prevV !== null) {
          flushBand(xForIdx(Math.min(iLast, this._targetValues.length - 1)) + 1, prevV);
        }
      }
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = "#f7d774";
      ctx.setLineDash([5, 4]);
      let tStarted = false;
      let prevTargetV = null;
      let prevTargetY = null;
      const targetTransitions = []; // {x, yFrom, yTo} for each step change
      ctx.beginPath();
      for (let i = iFirst; i <= iLast; i++) {
        if (i >= this._targetValues.length) break;
        const v = this._targetValues[i];
        if (!Number.isFinite(v)) { tStarted = false; prevTargetV = null; prevTargetY = null; continue; }
        const x = xForIdx(i);
        const y = yFor(v);
        // Detect a genuine setpoint change (>0.05°C) — not a pixel-level rounding difference.
        const valueJumped = prevTargetV !== null && Math.abs(v - prevTargetV) > 0.05;
        if (!tStarted || valueJumped) {
          if (valueJumped) targetTransitions.push({ x, yFrom: prevTargetY, yTo: y });
          ctx.moveTo(x, y); tStarted = true;
        } else { ctx.lineTo(x, y); }
        prevTargetV = v;
        prevTargetY = y;
      }
      ctx.stroke();

      // Draw solid yellow vertical lines at each step-change transition point.
      ctx.setLineDash([]);
      for (const tr of targetTransitions) {
        ctx.beginPath();
        ctx.moveTo(tr.x, Math.min(tr.yFrom, tr.yTo));
        ctx.lineTo(tr.x, Math.max(tr.yFrom, tr.yTo));
        ctx.stroke();
      }

      ctx.setLineDash([]);
      ctx.restore();
    }

    if (this._hoverX !== null && this._values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this._hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const index = Math.max(0, Math.min(n - 1, Math.round(visStart + ratio * (visEnd - visStart))));
      const value = this._values[index];
      const x = clampedX;
      const y = yFor(value);
      const hoverTime = elapsedSeconds * zoomStart + ratio * elapsedSeconds * (zoomEnd - zoomStart);
      const targetAtIdx = index < this._targetValues.length ? this._targetValues[index] : NaN;
      const targetStr = Number.isFinite(targetAtIdx) ? `  tgt:${targetAtIdx.toFixed(1)}` : "";
      const tip = `${value.toFixed(2)} C${targetStr}  T+${formatElapsed(Math.round(hoverTime))}`;

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
  static _signedOutput(sample) {
    if (sample.output_percent <= 0) {
      return 0;
    }
    return -Math.max(0, Math.min(1, sample.output_percent / 100));
  }

  static _signedRelay(sample) {
    if (sample.relay_on) return -1;
    if (sample.heat_on) return 1;
    return 0;
  }

  constructor(canvas) {
    this.canvas = canvas;
    this._values = [];
    this._hoverX = null;
    this._elapsedSeconds = null;
    this._rafId = null;
    // Hover is driven externally via setHoverRatio() broadcast from the section-level listener.
  }

  setValues(values) {
    this._values.splice(0, this._values.length, ...values);
    this._draw();
  }

  setElapsedSeconds(seconds) {
    this._elapsedSeconds = Number.isFinite(seconds) ? Math.max(0, seconds) : null;
    this._draw();
  }

  setHoverRatio(ratio) {
    if (ratio === null) {
      if (this._hoverX !== null) {
        this._hoverX = null;
        this._drawNow();
      }
      return;
    }
    const { axisPadLeft, pidPadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - pidPadRight);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._drawNow();
    }
  }

  _updateHover(clientX) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((clientX - rect.left) / rect.width) * this.canvas.width;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._draw();
    }
  }

  push(sample) {
    this._values.push(sample);
    this._draw();
  }

  clear() {
    this._values.length = 0;
    this._draw();
  }

  redraw() {
    this._draw();
  }

  _draw() {
    if (this._rafId !== null) {
      return;
    }
    this._rafId = window.requestAnimationFrame(() => {
      this._rafId = null;
      this._drawNow();
    });
  }

  _drawNow() {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;

    const { width, height } = this.canvas;
    ctx.clearRect(0, 0, width, height);

    if (this._values.length < 2) {
      drawNoData(ctx, width, height);
      return;
    }

    const { axisPadLeft, plotPadTop, plotPadBottom, pidPadRight: axisPadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, width - axisPadLeft - axisPadRight);
    const plotHeight = Math.max(1, height - plotPadTop - plotPadBottom);

    const leftSeries = [
      { color: "#40c4ff", value: (p) => p.pid_p_pct },
      { color: "#ffd740", value: (p) => p.pid_i_pct },
      { color: "#e040fb", value: (p) => p.pid_d_pct },
    ];
    const rightSeries = [
      { color: "#ff6d00", value: (p) => PidChart._signedOutput(p) },
    ];

    const n = this._values.length;
    const visStart = zoomStart * (n - 1);
    const visEnd = zoomEnd * (n - 1);
    const iFirst = Math.max(0, Math.floor(visStart));
    const iLast = Math.min(n - 1, Math.ceil(visEnd));
    const xForIdx = (i) => axisPadLeft + ((i - visStart) / Math.max(1, visEnd - visStart)) * plotWidth;
    let leftMin = Number.POSITIVE_INFINITY;
    let leftMax = Number.NEGATIVE_INFINITY;
    for (let i = iFirst; i <= iLast; i++) {
      const point = this._values[i];
      leftSeries.forEach((entry) => {
        const v = entry.value(point);
        if (v < leftMin) leftMin = v;
        if (v > leftMax) leftMax = v;
      });
    }
    const leftSpread = Math.max(0.1, leftMax - leftMin);
    // Add 10% padding above and below the PID term range.
    const leftPad = leftSpread * 0.10;
    leftMin -= leftPad;
    leftMax += leftPad;
    const leftPaddedSpread = leftMax - leftMin;
    const yForLeft = (v) => {
      const norm = (v - leftMin) / leftPaddedSpread;
      return height - plotPadBottom - norm * plotHeight;
    };
    const rightMin = -1;
    const rightMax = 1;
    const rightSpread = rightMax - rightMin;
    const yForRight = (v) => {
      const norm = (v - rightMin) / rightSpread;
      return height - plotPadBottom - norm * plotHeight;
    };

    const axisColor = "rgba(159, 180, 203, 0.35)";
    ctx.strokeStyle = axisColor;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(axisPadLeft, plotPadTop);
    ctx.lineTo(axisPadLeft, height - plotPadBottom);
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(width - axisPadRight, plotPadTop);
    ctx.lineTo(width - axisPadRight, height - plotPadBottom);
    ctx.stroke();

    const leftTickValues = [leftMax, leftMin + leftPaddedSpread / 2, leftMin];
    const leftDecimals = leftPaddedSpread >= 10 ? 0 : leftPaddedSpread >= 0.5 ? 1 : 2;
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    leftTickValues.forEach((tickValue) => {
      const label = tickValue.toFixed(leftDecimals);
      const y = yForLeft(tickValue);
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - axisPadRight, y);
      ctx.stroke();
      const textY = Math.max(14, Math.min(y + 4, height - 4));
      ctx.fillText(label, axisPadLeft - 4, textY);
    });

    const rightTicks = [
      { value: 1, label: "+1" },
      { value: 0, label: "0" },
      { value: -1, label: "-1" },
    ];
    ctx.fillStyle = "#ffffff";
    ctx.textAlign = "right";
    rightTicks.forEach((tick) => {
      const y = yForRight(tick.value);
      ctx.beginPath();
      ctx.moveTo(width - axisPadRight, y);
      ctx.lineTo(width - axisPadRight + 6, y);
      ctx.stroke();
      ctx.fillText(tick.label, width - 2, y + 4);
    });

    ctx.beginPath();
    ctx.moveTo(axisPadLeft, height - plotPadBottom);
    ctx.lineTo(width - axisPadRight, height - plotPadBottom);
    ctx.stroke();
    const elapsedSeconds = this._elapsedSeconds ?? ((this._values.length - 1) * TREND_SAMPLE_INTERVAL_SECONDS);
    ctx.save();
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.textAlign = "right";
    ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomEnd))}`, width - axisPadRight, height - 2);
    if (zoomStart > 0.001) {
      ctx.textAlign = "left";
      ctx.fillText(`T+${formatElapsed(Math.round(elapsedSeconds * zoomStart))}`, axisPadLeft + 4, height - 2);
    }
    ctx.restore();

    ctx.save();
    ctx.beginPath();
    ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
    ctx.clip();
    // Draw relay first so it sits behind all other traces.
    ctx.beginPath();
    ctx.lineWidth = 1.2;
    ctx.strokeStyle = "#69f0ae";
    for (let i = iFirst; i <= iLast; i++) {
      const x = xForIdx(i);
      const y = yForRight(PidChart._signedRelay(this._values[i]));
      if (i === iFirst) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, yForRight(PidChart._signedRelay(this._values[i - 1])));
        ctx.lineTo(x, y);
      }
    }
    ctx.stroke();
    leftSeries.forEach((entry) => {
      ctx.beginPath();
      ctx.lineWidth = 1.8;
      ctx.strokeStyle = entry.color;
      for (let i = iFirst; i <= iLast; i++) {
        const x = xForIdx(i);
        const y = yForLeft(entry.value(this._values[i]));
        if (i === iFirst) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      }
      ctx.stroke();
    });
    rightSeries.forEach((entry) => {
      ctx.beginPath();
      ctx.lineWidth = 1.8;
      ctx.strokeStyle = entry.color;
      for (let i = iFirst; i <= iLast; i++) {
        const x = xForIdx(i);
        const y = yForRight(entry.value(this._values[i]));
        if (i === iFirst) {
          ctx.moveTo(x, y);
        } else {
          ctx.lineTo(x, y);
        }
      }
      ctx.stroke();
    });
    ctx.restore();

    if (this._hoverX !== null && this._values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this._hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const i = Math.max(0, Math.min(n - 1, Math.round(visStart + ratio * (visEnd - visStart))));
      const sample = this._values[i];
      const x = clampedX;
      const hoverTime = elapsedSeconds * zoomStart + ratio * elapsedSeconds * (zoomEnd - zoomStart);
      const signedOutput = PidChart._signedOutput(sample);
      const relayMode = sample.relay_on ? "cool" : sample.heat_on ? "heat" : "off";
      const tip1 = `T+${formatElapsed(Math.round(hoverTime))}`;
      const tip2 = `P:${sample.pid_p_pct.toFixed(1)}% I:${sample.pid_i_pct.toFixed(1)}% D:${sample.pid_d_pct.toFixed(1)}%`;
      const tip3 = `drv:${signedOutput.toFixed(2)} win:${sample.window_step} on:${sample.on_steps} r:${relayMode}`;

      ctx.save();
      ctx.textAlign = "left";
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

// --- dashboard ---
let lastHistorySeq = -1;
let historyDropCount = 0; // packets missing from the stored JSONL, detected during replay
let collecting = false;
let syncCollectingUi = null;
let collectionToggleInFlight = false;
let pollRequestInFlight = false;
let loadedHistoryBaseSeconds = 0;
let lastUptimeSeconds = null;
let uptimeAtHistoryLoad = null;
// Set to true after each loadHistoryFromDevice so the first merged point gets a
// session-boundary gap marker, clearly separating persisted history from live data.
let sessionGapPending = false;
// When true, the chart auto-scrolls so the latest data is always at the right
// edge. Set to false when the user manually zooms/pans; double-clicking any
// chart canvas re-enables it.
let liveFollow = true;

const loadHistoryFromDevice = async (sparklines, pidChart) => {
  const response = await fetch(`/history?points=${HISTORY_LOAD_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json());
  const points = Array.isArray(payload.points) ? payload.points : [];

  // Clear all charts first — we replay every record from scratch via push(),
  // exactly as if each packet had just arrived from the device.
  sparklines.forEach((sl) => sl.clear());
  pidChart.clear();
  lastHistorySeq = -1;
  historyDropCount = 0;

  const sampleIntervalS =
    Number.isFinite(Number(payload.sample_interval_s)) && Number(payload.sample_interval_s) > 0
      ? Number(payload.sample_interval_s)
      : TREND_SAMPLE_INTERVAL_SECONDS;
  // A seq jump larger than 1.5× the normal interval means a real gap (reboot / no coverage).
  const maxExpectedSeqGap = 1.5 * sampleIntervalS;
  let prevSeq = null;
  let pointCount = 0;

  points.forEach((point) => {
    if (!Array.isArray(point) || point.length < 7) return;
    const seq = Number(point[0]);

    // Mark a gap on all sparklines if seq jumped unexpectedly.
    if (prevSeq !== null) {
      const seqDiff = seq - prevSeq;
      if (seqDiff < 0 || seqDiff > maxExpectedSeqGap) {
        sparklines.forEach((sl) => sl.markGapBeforeNext());
        // Count the missing packets (backward jump counts as 1).
        if (seqDiff > maxExpectedSeqGap) historyDropCount += Math.round(seqDiff - 1);
        else historyDropCount += 1;
      }
    }
    prevSeq = seq;
    lastHistorySeq = seq;
    pointCount++;

    const temp   = Number(point[1]);
    const target = Number(point[2]);

    const primary = sparklines.get(0);
    if (primary) {
      primary.push(temp);
      primary.pushTarget(target);
    }

    // Extra sensor temps: column 7 → sensor index 1, column 8 → sensor index 2.
    // Columns 9–11 are pid_p/i/d — stop before them.
    for (let col = 7; col < Math.min(point.length, 9); col++) {
      const sensorIdx = col - 6;
      const sl = sparklines.get(sensorIdx);
      if (sl) {
        const raw = point[col];
        sl.push(raw != null ? Number(raw) : NaN);
      }
    }

    pidChart.push({
      target_c: target,
      pid_p_pct: Number(point[9]) || 0,
      pid_i_pct: Number(point[10]) || 0,
      pid_d_pct: Number(point[11]) || 0,
      output_percent: Number(point[3]),
      window_step:   Number(point[4]),
      on_steps:      Number(point[5]),
      relay_on:      Number(point[6]) !== 0,
    });
  });

  loadedHistoryBaseSeconds = Math.max(0, pointCount - 1) * sampleIntervalS;
  uptimeAtHistoryLoad = null; // reset; captured on next updateFromStatus call
  sessionGapPending = true;  // gap marker inserted before first new live point

  sparklines.forEach((sl) => sl.setElapsedSeconds(loadedHistoryBaseSeconds));
  pidChart.setElapsedSeconds(loadedHistoryBaseSeconds);
};

const mergeHistoryFromDevice = async (sparklines, pidChart) => {
  const response = await fetch(`/history?points=${HISTORY_MERGE_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json());
  const points = Array.isArray(payload.points) ? payload.points : [];
  const primarySparkline = sparklines.get(0);
  if (!primarySparkline) return;

  let prevSeq = lastHistorySeq;
  points.forEach((point) => {
    if (!Array.isArray(point) || point.length < 7) {
      return;
    }
    const seq = Number(point[0]);
    if (!Number.isFinite(seq) || seq <= lastHistorySeq) {
      return;
    }

    prevSeq = seq;
    lastHistorySeq = seq;
    primarySparkline.push(Number(point[1]));
    primarySparkline.pushTarget(Number(point[2]));
    // Extra sensor temps: column 7 → sensor 1, column 8 → sensor 2.
    // Columns 9–11 are pid_p/i/d — stop before them.
    for (let col = 7; col < Math.min(point.length, 9); col++) {
      const sensorIdx = col - 6;
      const sl = sparklines.get(sensorIdx);
      if (sl) {
        const raw = point[col];
        sl.push(raw != null ? Number(raw) : NaN);
      }
    }
    pidChart.push({
      target_c: Number(point[2]),
      pid_p_pct: Number(point[9]) || 0,
      pid_i_pct: Number(point[10]) || 0,
      pid_d_pct: Number(point[11]) || 0,
      output_percent: Number(point[3]),
      window_step: Number(point[4]),
      on_steps: Number(point[5]),
      relay_on: Number(point[6]) !== 0,
    });
  });
};

const updateFromStatus = (
  data,
  sparklines,
  pidCharts,
  setControlProbeIndex,
  setTempProbeLabel,
  setPidProbeLabel,
  ensureTemperatureCharts,
) => {
  setControlProbeIndex(data.control_probe_index);

  if (typeof data.system.collecting === "boolean") {
    if (syncCollectingUi) {
      syncCollectingUi(data.system.collecting);
    } else {
      collecting = data.system.collecting;
    }
  }

  lastUptimeSeconds = data.system.uptime_s;
  if (collecting) {
    if (uptimeAtHistoryLoad === null) uptimeAtHistoryLoad = data.system.uptime_s;
    const totalElapsed = loadedHistoryBaseSeconds + (data.system.uptime_s - uptimeAtHistoryLoad);
    sparklines.forEach((sparkline) => {
      sparkline.setElapsedSeconds(totalElapsed);
    });
    pidCharts.forEach((pidChart) => {
      pidChart.setElapsedSeconds(totalElapsed);
    });
  }

  setText("title", `${data.device.toUpperCase()} CONTROL PANEL`);
  const hostnameEl = document.getElementById("device-hostname");
  if (hostnameEl) hostnameEl.textContent = data.hostname ?? "";
  setText("updated", new Date().toLocaleTimeString());
  if (data.system && data.system.uptime_s !== null) {
    setText("uptime", formatUptime(data.system.uptime_s));
  }
  const dropped = (data.system.packets_dropped ?? 0) + historyDropCount;
  const seqEl = document.getElementById("seq-info");
  if (seqEl) {
    const dropLabel = historyDropCount > 0
      ? `drops: ${data.system.packets_dropped ?? 0} live + ${historyDropCount} stored`
      : `drops: ${data.system.packets_dropped ?? 0}`;
    seqEl.textContent = `seq: ${data.system.seq ?? "--"}  ${dropLabel}`;
    seqEl.style.color = dropped > 0 ? "var(--warn)" : "";
  }
  ensureTemperatureCharts(data.sensors || []);

  const primarySensor = data.sensors && data.sensors.length > 0 ? data.sensors[0] : null;
  if (primarySensor) {
    setText("temp", formatTemp(primarySensor.temperature_c, "C"));
    setText("temp-secondary", formatTemp(primarySensor.temperature_f, "F"));
  }

  // First live push after a history load: mark a session boundary on all sparklines.
  if (collecting && sessionGapPending) {
    sessionGapPending = false;
    sparklines.forEach((sl) => sl.markGapBeforeNext());
  }

  if (Array.isArray(data.sensors)) {
    data.sensors.forEach((sensor) => {
      setTempProbeLabel(sensor.index, sensor.name);
      setPidProbeLabel(sensor.index, sensor.name);

      if (collecting && sensor.temperature_c !== null) {
        const sensorChart = sparklines.get(sensor.index);
        if (sensorChart) {
          sensorChart.push(sensor.temperature_c);
          if (sensor.index === 0) {
            sensorChart.pushTarget(data.pid.target_c);
            sensorChart.setDeadband(data.pid.deadband_c ?? 0);
          }
        }
      }
    });
  }

  setText("target", `${data.pid.target_c.toFixed(1)} C`);
  setText("target-secondary", `${data.pid.target_f.toFixed(1)} F`);
  const targetInput = byId("target-input");
  if (document.activeElement !== targetInput) {
    targetInput.value = data.pid.target_c.toFixed(1);
  }

  const isHeating = collecting && data.pid.heat_on === true;
  const isCooling = collecting && data.pid.relay_on === true;
  const pidPct = collecting ? `${data.pid.output_percent.toFixed(1)}%` : "0.0%";
  byId("pid").textContent = pidPct;
  byId("pid").style.color = "";
  const modeEl = document.getElementById("pid-mode");
  if (modeEl) {
    modeEl.textContent = isHeating ? "Heating" : isCooling ? "Cooling" : collecting ? "Idle" : "--";
    modeEl.style.color = isHeating ? "#ffb347" : isCooling ? "#40c4ff" : "";
  }
  const relayOn = isHeating || isCooling;
  const relayState = !collecting ? "Deactivated" : (relayOn ? "On" : "Off");
  setText("relay", relayState);
  byId("relay").style.color = relayState === "Deactivated" ? "#ff6e6e" : "";
  const relayModeEl = document.getElementById("relay-mode");
  if (relayModeEl) {
    if (!collecting) {
      relayModeEl.textContent = "--";
      relayModeEl.style.color = "";
    } else if (isHeating) {
      relayModeEl.textContent = "Heat";
      relayModeEl.style.color = "#ffb347";
    } else if (isCooling) {
      relayModeEl.textContent = "Cool";
      relayModeEl.style.color = "#40c4ff";
    } else {
      relayModeEl.textContent = "Idle";
      relayModeEl.style.color = "";
    }
  }

  if (collecting) {
    const sample = {
      target_c: data.pid.target_c,
      pid_p_pct: data.pid.pid_p_pct ?? 0,
      pid_i_pct: data.pid.pid_i_pct ?? 0,
      pid_d_pct: data.pid.pid_d_pct ?? 0,
      output_percent: data.pid.output_percent,
      window_step: data.pid.window_step,
      on_steps: data.pid.on_steps,
      relay_on: data.pid.relay_on ? 1 : 0,
      heat_on: data.pid.heat_on ? 1 : 0,
    };
    pidCharts.forEach((pidChart) => {
      pidChart.push(sample);
    });
  }

  // Auto-scroll: when live-following, keep LIVE_WINDOW_SECONDS pinned to the
  // right edge. The already-scheduled RAF picks up the new zoom automatically.
  if (collecting && liveFollow) {
    const totalElapsed = loadedHistoryBaseSeconds +
      (uptimeAtHistoryLoad !== null ? Math.max(0, data.system.uptime_s - uptimeAtHistoryLoad) : 0);
    if (totalElapsed > LIVE_WINDOW_SECONDS) {
      setZoomWindow(Math.max(0, 1 - LIVE_WINDOW_SECONDS / totalElapsed), 1);
    }
  }

  setText("ip", data.system.ip || "--");
  updateNtpPill(data.system.ntp.synced);

  // Device IP stat: make it a clickable link when a real IP is available.
  const deviceIp = data.system.ip || "";
  const ipValid = deviceIp !== "" && !deviceIp.startsWith("Error") && deviceIp !== "0.0.0.0";
  const devicePort = data.system.device_http_port ?? 80;
  const ipEl = byId("ip");
  if (ipValid) {
    ipEl.href = devicePort === 80 ? `http://${deviceIp}/` : `http://${deviceIp}:${devicePort}/`;
  } else {
    ipEl.removeAttribute("href");
  }
};

const loop = async (
  sparklines,
  pidCharts,
  primaryPidChart,
  setControlProbeIndex,
  setTempProbeLabel,
  setPidProbeLabel,
  ensureTemperatureCharts,
) => {
  if (collectionToggleInFlight || pollRequestInFlight) {
    return;
  }

  pollRequestInFlight = true;
  try {
    // Merge catch-up history BEFORE fetching live status so that any points
    // between the last downsampled history point and now are appended in
    // chronological order, before the gap marker and live status push.
    if (collecting) {
      await mergeHistoryFromDevice(sparklines, primaryPidChart);
    }
    const response = await fetch("/status", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = (await response.json());
    updateFromStatus(
      payload,
      sparklines,
      pidCharts,
      setControlProbeIndex,
      setTempProbeLabel,
      setPidProbeLabel,
      ensureTemperatureCharts,
    );
  } catch (error) {
    const msg = error instanceof TypeError ? "No link \u2014 retrying\u2026" : `Update failed: ${String(error)}`;
    setText("updated", msg);
    const pill = byId("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  } finally {
    pollRequestInFlight = false;
  }
};

const start = () => {
  const sparklines = new Map();
  const pidCharts = new Map();
  const tempLabelEls = new Map();
  const pidLabelEls = new Map();
  let controlProbeIndex = 0;

  const primaryCanvas = document.getElementById("temp-chart-0");
  const primaryCard = primaryCanvas?.closest("article");

  const pidCanvas = document.getElementById("pid-chart-0");
  const pidCard = pidCanvas?.closest("article");
  const section = document.querySelector("section");

  if (!pidCanvas || !primaryCanvas || !primaryCard || !pidCard || !section) {
    setText("updated", "Error: Dashboard layout mismatch");
    return;
  }

  primaryCard.dataset.sensorIndex = "0";
  primaryCard.dataset.cardType = "temp";
  pidCard.dataset.sensorIndex = "0";
  pidCard.dataset.cardType = "pid";

  const buildTempLegend = (card, { showTarget = true, showGap = true } = {}) => {
    let el = card.querySelector(".chart-legend");
    if (!el) { el = document.createElement("div"); el.className = "chart-legend"; card.appendChild(el); }
    el.innerHTML =
      `<span class="legend-item"><span class="legend-swatch" style="background:linear-gradient(90deg,#40c4ff,#40d990)"></span>Temperature</span>` +
      (showTarget ? `<span class="legend-item"><span class="legend-swatch" style="background:#f7d774"></span>Target \u00b0C</span>` : "") +
      (showGap ? `<span class="legend-item"><span class="legend-swatch" style="background:rgba(255,80,80,0.75)"></span>Missing data</span>` : "");
  };

  const buildPidLegend = (card) => {
    let el = card.querySelector(".chart-legend");
    if (!el) { el = document.createElement("div"); el.className = "chart-legend"; card.appendChild(el); }
    el.innerHTML = [
      ["#40c4ff", "Kp"], ["#ffd740", "Ki"], ["#e040fb", "Kd"],
      ["#ff6d00", "Output%"], ["#69f0ae", "Relay"],
    ].map(([color, label]) =>
      `<span class="legend-item"><span class="legend-swatch" style="background:${color}"></span>${label}</span>`
    ).join("");
  };

  sparklines.set(0, new Sparkline(primaryCanvas));
  pidCharts.set(0, new PidChart(pidCanvas));

  const primaryChart = sparklines.get(0);
  const primaryPidChart = pidCharts.get(0);
  const primaryTempLabel = document.getElementById("temp-chart-probe-0");
  const primaryPidLabel = document.getElementById("pid-chart-probe-0");
  if (primaryTempLabel) {
    tempLabelEls.set(0, primaryTempLabel);
  }
  if (primaryPidLabel) {
    pidLabelEls.set(0, primaryPidLabel);
  }
  buildTempLegend(primaryCard);
  buildPidLegend(pidCard);

  const setControlProbeIndex = (nextIndex) => {
    if (!Number.isFinite(nextIndex)) {
      return;
    }
    const normalized = Math.max(0, Math.floor(nextIndex));
    if (normalized === controlProbeIndex) {
      return;
    }

    const primaryPid = pidCharts.get(controlProbeIndex);
    if (primaryPid) {
      pidCharts.delete(controlProbeIndex);
      pidCharts.set(normalized, primaryPid);
    }

    const label = pidLabelEls.get(controlProbeIndex);
    if (label) {
      pidLabelEls.delete(controlProbeIndex);
      pidLabelEls.set(normalized, label);
    }

    pidCard.dataset.sensorIndex = String(normalized);
    controlProbeIndex = normalized;
  };

  const setLabelIfChanged = (el, next) => {
    if (!el) {
      return;
    }
    if (el.textContent !== next) {
      el.textContent = next;
    }
  };

  const setTempProbeLabel = (index, label) => {
    const next = label || "--";
    const cached = tempLabelEls.get(index);
    if (cached) {
      setLabelIfChanged(cached, next);
      return;
    }
    const found = document.getElementById(`temp-chart-probe-${index}`);
    if (found) {
      tempLabelEls.set(index, found);
      setLabelIfChanged(found, next);
    }
  };

  const setPidProbeLabel = (index, label) => {
    const next = label || "--";
    const cached = pidLabelEls.get(index);
    if (cached) {
      setLabelIfChanged(cached, next);
      return;
    }
    const found = document.getElementById(`pid-chart-probe-${index}`);
    if (found) {
      pidLabelEls.set(index, found);
      setLabelIfChanged(found, next);
    }
  };

  const hoverRatioForClientX = (canvas, clientX) => {
    const rect = canvas.getBoundingClientRect();
    const canvasX = ((clientX - rect.left) / rect.width) * canvas.width;
    const { axisPadLeft, sparklinePadRight, pidPadRight } = CHART_LAYOUT;
    const isPid = canvas.closest("article")?.dataset.cardType === "pid";
    const rightPad = isPid ? pidPadRight : sparklinePadRight;
    const plotWidth = Math.max(1, canvas.width - axisPadLeft - rightPad);
    return Math.max(0, Math.min(1, (canvasX - axisPadLeft) / plotWidth));
  };

  // Broadcast hover ratio to all charts for synchronized crosshair tracking.
  const broadcastHoverRatio = (ratio) => {
    sparklines.forEach((s) => s.setHoverRatio(ratio));
    pidCharts.forEach((c) => c.setHoverRatio(ratio));
  };
  // Keep last ratio so the cursor stays alive when the mouse crosses legend/header
  // gaps between canvases (internal canvas listeners are gone so no race possible).
  let lastHoverRatio = null;
  section.addEventListener("mousemove", (event) => {
    const canvas = event.target?.closest("canvas.chart");
    if (canvas instanceof HTMLCanvasElement) {
      lastHoverRatio = hoverRatioForClientX(canvas, event.clientX);
      broadcastHoverRatio(lastHoverRatio);
    } else if (lastHoverRatio !== null) {
      broadcastHoverRatio(lastHoverRatio);
    }
  });
  section.addEventListener("mouseleave", () => { lastHoverRatio = null; broadcastHoverRatio(null); });

  const applyZoom = (pivotRatio, factor) => {
    liveFollow = false;
    const span = zoomEnd - zoomStart;
    const newSpan = Math.max(0.02, Math.min(1, span * factor));
    const center = zoomStart + pivotRatio * span;
    let newStart = Math.max(0, center - pivotRatio * newSpan);
    let newEnd = newStart + newSpan;
    if (newEnd > 1) { newEnd = 1; newStart = Math.max(0, 1 - newSpan); }
    setZoomWindow(newStart, newEnd);
    sparklines.forEach((sparkline) => sparkline.redraw());
    pidCharts.forEach((chart) => chart.redraw());
  };

  const applyPan = (delta) => {
    liveFollow = false;
    const span = zoomEnd - zoomStart;
    const newStart = Math.max(0, Math.min(1 - span, zoomStart + delta * span));
    setZoomWindow(newStart, newStart + span);
    sparklines.forEach((sparkline) => sparkline.redraw());
    pidCharts.forEach((chart) => chart.redraw());
  };

  const onWheelZoom = (canvas, e) => {
    e.preventDefault();
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY) * 0.3) {
      applyPan(e.deltaX / 800);
    } else if (e.deltaY !== 0) {
      const ratio = hoverRatioForClientX(canvas, e.clientX);
      applyZoom(ratio, e.deltaY > 0 ? 1.25 : 0.8);
    }
  };

  const resetZoom = () => {
    liveFollow = true;
    // Snap immediately to the live window (or full range if data is shorter).
    const elapsed = loadedHistoryBaseSeconds +
      (uptimeAtHistoryLoad !== null && lastUptimeSeconds !== null
        ? Math.max(0, lastUptimeSeconds - uptimeAtHistoryLoad) : 0);
    if (elapsed > LIVE_WINDOW_SECONDS) {
      setZoomWindow(Math.max(0, 1 - LIVE_WINDOW_SECONDS / elapsed), 1);
    } else {
      setZoomWindow(0, 1);
    }
    sparklines.forEach((sparkline) => sparkline.redraw());
    pidCharts.forEach((chart) => chart.redraw());
  };

  section.addEventListener("wheel", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) {
      return;
    }
    const canvas = target.closest("canvas.chart");
    if (!(canvas instanceof HTMLCanvasElement)) {
      return;
    }
    onWheelZoom(canvas, event);
  }, { passive: false });

  section.addEventListener("dblclick", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) {
      return;
    }
    const canvas = target.closest("canvas.chart");
    if (!(canvas instanceof HTMLCanvasElement)) {
      return;
    }
    resetZoom();
  });

  const createTempCard = (index, name) => {
    const newTempCard = primaryCard.cloneNode(true);
    newTempCard.dataset.sensorIndex = String(index);
    newTempCard.dataset.cardType = "temp";

    const tempNameEl = newTempCard.querySelector(".chart-title-left");
    if (tempNameEl) {
      tempNameEl.id = `temp-chart-probe-${index}`;
      tempLabelEls.set(index, tempNameEl);
      setLabelIfChanged(tempNameEl, name || `probe-${index + 1}`);
    }

    const tempCanvas = newTempCard.querySelector("canvas.chart");
    if (!tempCanvas) {
      return false;
    }
    tempCanvas.id = `temp-chart-${index}`;
    tempCanvas.width = CHART_CANVAS_WIDTH;
    tempCanvas.height = CHART_CANVAS_HEIGHT;

    const sparkline = new Sparkline(tempCanvas, { showTarget: false });
    sparklines.set(index, sparkline);
    sparkline.setElapsedSeconds(loadedHistoryBaseSeconds);

    section.appendChild(newTempCard);
    buildTempLegend(newTempCard, { showTarget: false, showGap: false });
    return true;
  };

  const createPidCard = (index, name) => {
    const newPidCard = pidCard.cloneNode(true);
    newPidCard.dataset.sensorIndex = String(index);
    newPidCard.dataset.cardType = "pid";

    const pidNameEl = newPidCard.querySelector(".chart-title-left");
    if (pidNameEl) {
      pidNameEl.id = `pid-chart-probe-${index}`;
      pidLabelEls.set(index, pidNameEl);
      setLabelIfChanged(pidNameEl, name || `probe-${index + 1}`);
    }

    const newPidCanvas = newPidCard.querySelector("canvas.chart");
    if (!newPidCanvas) {
      return false;
    }
    newPidCanvas.id = `pid-chart-${index}`;
    newPidCanvas.width = CHART_CANVAS_WIDTH;
    newPidCanvas.height = CHART_CANVAS_HEIGHT;

    const chart = new PidChart(newPidCanvas);
    chart.setElapsedSeconds(loadedHistoryBaseSeconds);
    pidCharts.set(index, chart);

    section.appendChild(newPidCard);
    buildPidLegend(newPidCard);
    return true;
  };

  const ensureTemperatureCharts = (sensors) => {
    if (!Array.isArray(sensors)) {
      return;
    }

    const sortedSensors = [...sensors]
      .map((sensor) => ({ ...sensor, index: Number(sensor.index) }))
      .filter((sensor) => Number.isFinite(sensor.index) && sensor.index >= 0)
      .sort((a, b) => a.index - b.index);

    let cardsChanged = false;

    sortedSensors.forEach((sensor) => {
      const index = sensor.index;
      if (index === 0) {
        return;
      }

      if (!sparklines.has(index)) {
        cardsChanged = createTempCard(index, sensor.name) || cardsChanged;
      }

      // Only create PID card if this is the control probe
      if (index === controlProbeIndex && !pidCharts.has(index)) {
        cardsChanged = createPidCard(index, sensor.name) || cardsChanged;
      }
    });

    if (!cardsChanged) {
      return;
    }

    const tempCards = [];
    section.querySelectorAll("article[data-card-type]").forEach((el) => {
      const article = el;
      if (article.dataset.cardType === "temp") tempCards.push(article);
    });

    tempCards.forEach((el) => {
      const idx = Number(el.dataset.sensorIndex);
      const pidCardEl = section.querySelector(
        `article[data-card-type='pid'][data-sensor-index='${idx}']`,
      );
      if (pidCardEl) {
        section.insertBefore(el, pidCardEl);
      }
    });
  };

  const targetInput = byId("target-input");
  const targetSubmit = byId("target-submit");

  const applyTarget = async () => {
    const parsed = Number.parseFloat(targetInput.value);
    if (!Number.isFinite(parsed)) {
      setTargetFeedback("Enter a valid number", "error");
      return;
    }
    if (parsed < -20 || parsed > 100) {
      setTargetFeedback("Target must be between -20 and 100 C", "error");
      return;
    }

    targetSubmit.disabled = true;
    setTargetFeedback("Applying target...");
    try {
      await submitTargetTemperature(parsed);
      setTargetFeedback(`Applied ${parsed.toFixed(1)} C`, "ok");
      await loop(
        sparklines,
        pidCharts,
        primaryPidChart,
        setControlProbeIndex,
        setTempProbeLabel,
        setPidProbeLabel,
        ensureTemperatureCharts,
      );
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

  loadHistoryFromDevice(sparklines, primaryPidChart)
    .catch((error) => {
      const msg = error instanceof TypeError ? "No link \u2014 retrying\u2026" : `History load failed: ${String(error)}`;
      setText("updated", msg);
    })
    .finally(() => {
      void loop(
        sparklines,
        pidCharts,
        primaryPidChart,
        setControlProbeIndex,
        setTempProbeLabel,
        setPidProbeLabel,
        ensureTemperatureCharts,
      );
    });
  window.setInterval(() => {
    void loop(
      sparklines,
      pidCharts,
      primaryPidChart,
      setControlProbeIndex,
      setTempProbeLabel,
      setPidProbeLabel,
      ensureTemperatureCharts,
    );
  }, 1000);

  // SSE: update immediately when the server receives a UDP packet.
  // Falls back to the 1-second interval if /events is unavailable (e.g. on the ESP32).
  const evtSrc = new EventSource("/events");
  let sseOpened = false;
  evtSrc.onopen = () => { sseOpened = true; };
  evtSrc.onerror = () => { if (!sseOpened) evtSrc.close(); };
  evtSrc.addEventListener("pkt", () => {
    void loop(
      sparklines,
      pidCharts,
      primaryPidChart,
      setControlProbeIndex,
      setTempProbeLabel,
      setPidProbeLabel,
      ensureTemperatureCharts,
    );
  });

  const menuBtn = byId("menu-btn");
  const menuDropdown = byId("menu-dropdown");
  const clearDataBtn = byId("clear-data");
  const startDataBtn = byId("start-data");
  const stopDataBtn = byId("stop-data");

  const setCollecting = (value) => {
    collecting = value;
    startDataBtn.disabled = value;
    stopDataBtn.disabled = !value;
    const dot = document.getElementById("collect-dot");
    if (dot) dot.classList.toggle("active", value);
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
    void setCollectionOnDevice(true, () => pollRequestInFlight)
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
    void setCollectionOnDevice(false, () => pollRequestInFlight)
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
    if (!confirm("Clear all history? This cannot be undone.")) { return; }
    clearHistoryOnDevice()
      .then(() => {
        sparklines.forEach((sparkline) => sparkline.clear());
        pidCharts.forEach((chart) => chart.clear());
        lastUptimeSeconds = null;
        loadedHistoryBaseSeconds = 0;
        uptimeAtHistoryLoad = null;
        lastHistorySeq = -1;
        historyDropCount = 0;
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
