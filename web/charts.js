import { formatElapsed } from "./ui.js";

export const TREND_SAMPLE_INTERVAL_SECONDS = 2;
export const CHART_CANVAS_WIDTH = 1120;
export const CHART_CANVAS_HEIGHT = 220;
export const NO_DATA_FONT = "700 20px 'Avenir Next', 'Trebuchet MS', sans-serif";

export const CHART_LAYOUT = {
  axisPadLeft: 46,
  plotPadTop: 8,
  plotPadBottom: 8,
  sparklinePadRight: 6,
  pidPadRight: 24,
};

// Zoom window state: owned here so chart drawNow() can read without coupling to app module.
export let zoomStart = 0;
export let zoomEnd = 1;

export const setZoomWindow = (start, end) => {
  zoomStart = start;
  zoomEnd = end;
};

export const drawNoData = (ctx, width, height) => {
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

export class Sparkline {
  constructor(canvas) {
    this.canvas = canvas;
    this._values = [];
    this._hoverX = null;
    this._elapsedSeconds = null;
    this._rafId = null;
    this.canvas.addEventListener("mousemove", (event) => {
      this._updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this._hoverX = null;
      this._draw();
    });
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
        this._draw();
      }
      return;
    }
    const { axisPadLeft, sparklinePadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - sparklinePadRight);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._draw();
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
    this._values.push(value);
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
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    tickValues.forEach((tickValue) => {
      const y = yFor(tickValue);
      ctx.strokeStyle = axisColor;
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - 4, y);
      ctx.stroke();
      ctx.fillText(`${tickValue.toFixed(1)}°C`, axisPadLeft - 4, y + 4);
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

    ctx.save();
    ctx.beginPath();
    ctx.rect(axisPadLeft, plotPadTop, plotWidth, plotHeight);
    ctx.clip();
    ctx.lineWidth = 2;
    ctx.strokeStyle = gradient;
    ctx.beginPath();
    for (let i = iFirst; i <= iLast; i++) {
      const x = xForIdx(i);
      const y = yFor(this._values[i]);
      if (i === iFirst) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, y);
      }
    }
    ctx.stroke();
    ctx.restore();

    if (this._hoverX !== null && this._values.length > 0) {
      const clampedX = Math.max(axisPadLeft, Math.min(axisPadLeft + plotWidth, this._hoverX));
      const ratio = (clampedX - axisPadLeft) / plotWidth;
      const index = Math.max(0, Math.min(n - 1, Math.round(visStart + ratio * (visEnd - visStart))));
      const value = this._values[index];
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

export class PidChart {
  static _signedOutput(sample) {
    if (sample.output_percent <= 0) {
      return 0;
    }
    return -Math.max(0, Math.min(1, sample.output_percent / 100));
  }

  static _signedRelay(sample) {
    return sample.relay_on ? -1 : 0;
  }

  constructor(canvas) {
    this.canvas = canvas;
    this._values = [];
    this._hoverX = null;
    this._elapsedSeconds = null;
    this._rafId = null;
    this.canvas.addEventListener("mousemove", (event) => {
      this._updateHover(event.clientX);
    });
    this.canvas.addEventListener("mouseleave", () => {
      this._hoverX = null;
      this._draw();
    });
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
        this._draw();
      }
      return;
    }
    const { axisPadLeft, pidPadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, this.canvas.width - axisPadLeft - pidPadRight);
    const clampedRatio = Math.max(0, Math.min(1, ratio));
    const x = axisPadLeft + clampedRatio * plotWidth;
    if (this._hoverX !== x) {
      this._hoverX = x;
      this._draw();
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
      { color: "#f7d774", value: (p) => p.target_c },
      { color: "#6ec5ff", value: (p) => p.kp },
      { color: "#8ef0c8", value: (p) => p.ki },
      { color: "#b28cff", value: (p) => p.kd },
      { color: "#7cf3ff", value: (p) => p.window_step },
      { color: "#ffb3d1", value: (p) => p.on_steps },
    ];
    const rightSeries = [
      { color: "#ff8d6e", value: (p) => PidChart._signedOutput(p) },
      { color: "#ffffff", value: (p) => PidChart._signedRelay(p) },
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
    const yForLeft = (v) => {
      const norm = (v - leftMin) / leftSpread;
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

    const leftTickValues = [leftMax, leftMin + leftSpread / 2, leftMin];
    ctx.font = "12px 'Avenir Next', 'Trebuchet MS', sans-serif";
    ctx.fillStyle = "rgba(230, 241, 255, 0.82)";
    ctx.textAlign = "right";
    leftTickValues.forEach((tickValue) => {
      const y = yForLeft(tickValue);
      ctx.beginPath();
      ctx.moveTo(axisPadLeft, y);
      ctx.lineTo(width - axisPadRight, y);
      ctx.stroke();
      ctx.fillText(`${tickValue.toFixed(1)}°C`, axisPadLeft - 4, y + 4);
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
    rightSeries.forEach((entry, idx) => {
      ctx.beginPath();
      ctx.lineWidth = idx === rightSeries.length - 1 ? 1.2 : 1.8;
      ctx.strokeStyle = entry.color;
      for (let i = iFirst; i <= iLast; i++) {
        const x = xForIdx(i);
        const y = yForRight(entry.value(this._values[i]));
        if (i === iFirst) {
          ctx.moveTo(x, y);
        } else {
          if (idx === rightSeries.length - 1) {
            ctx.lineTo(x, yForRight(entry.value(this._values[i - 1])));
            ctx.lineTo(x, y);
          } else {
            ctx.lineTo(x, y);
          }
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
      const relayMode = sample.relay_on ? "cool" : "off";
      const tip1 = `T+${formatElapsed(Math.round(hoverTime))}`;
      const tip2 = `t:${sample.target_c.toFixed(1)} kp:${sample.kp.toFixed(2)} ki:${sample.ki.toFixed(2)} kd:${sample.kd.toFixed(2)}`;
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
