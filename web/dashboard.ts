import { byId, setText, formatTemp, formatUptime, setTargetFeedback, updateNtpPill } from "./ui.js";
import { HISTORY_FETCH_POINTS, submitTargetTemperature, clearHistoryOnDevice, setCollectionOnDevice } from "./api.js";
import type { StatusPayload, PidSample, HistoryPayload } from "./api.js";
import { Sparkline, PidChart, CHART_CANVAS_WIDTH, CHART_CANVAS_HEIGHT, CHART_LAYOUT, TREND_SAMPLE_INTERVAL_SECONDS, zoomStart, zoomEnd, setZoomWindow } from "./charts.js";

let lastHistorySeq = -1;
let collecting = false;
let syncCollectingUi: ((value: boolean) => void) | null = null;
let collectionToggleInFlight = false;
let pollRequestInFlight = false;
let loadedHistoryBaseSeconds = 0;
let lastUptimeSeconds: number | null = null;

const loadHistoryFromDevice = async (sparklines: Map<number, Sparkline>, pidChart: PidChart): Promise<void> => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json()) as HistoryPayload;
  const points = Array.isArray(payload.points) ? payload.points : [];
  const tempValues: number[] = [];
  const pidValues: PidSample[] = [];
  // Keyed by sensor index (1-based); extra sensor temps from history columns 7+
  const extraTempValues: Map<number, number[]> = new Map();

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
    // Extra sensor temps: column 7 → sensor 1, column 8 → sensor 2, etc.
    for (let col = 7; col < point.length; col++) {
      const sensorIdx = col - 6;
      if (!extraTempValues.has(sensorIdx)) extraTempValues.set(sensorIdx, []);
      const raw = point[col];
      extraTempValues.get(sensorIdx)!.push(raw != null ? Number(raw) : NaN);
    }
    lastHistorySeq = Number(point[0]);
  });

  const sampleIntervalS =
    Number.isFinite(Number(payload.sample_interval_s)) && Number(payload.sample_interval_s) > 0
      ? Number(payload.sample_interval_s)
      : TREND_SAMPLE_INTERVAL_SECONDS;
  loadedHistoryBaseSeconds = Math.max(0, tempValues.length - 1) * sampleIntervalS;

  const primarySparkline = sparklines.get(0);
  if (primarySparkline) {
    primarySparkline.setValues(tempValues);
    primarySparkline.setElapsedSeconds(loadedHistoryBaseSeconds);
  }
  for (const [sensorIdx, values] of extraTempValues) {
    const sl = sparklines.get(sensorIdx);
    if (sl) {
      sl.setValues(values);
      sl.setElapsedSeconds(loadedHistoryBaseSeconds);
    }
  }
  pidChart.setValues(pidValues);
  pidChart.setElapsedSeconds(loadedHistoryBaseSeconds);
};

const mergeHistoryFromDevice = async (sparklines: Map<number, Sparkline>, pidChart: PidChart): Promise<void> => {
  const response = await fetch(`/history?points=${HISTORY_FETCH_POINTS}`, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  const payload = (await response.json()) as HistoryPayload;
  const points = Array.isArray(payload.points) ? payload.points : [];
  const primarySparkline = sparklines.get(0);
  if (!primarySparkline) return;

  points.forEach((point) => {
    if (!Array.isArray(point) || point.length < 7) {
      return;
    }
    const seq = Number(point[0]);
    if (!Number.isFinite(seq) || seq <= lastHistorySeq) {
      return;
    }
    lastHistorySeq = seq;
    primarySparkline.push(Number(point[1]));
    // Extra sensor temps: column 7 → sensor 1, column 8 → sensor 2, etc.
    for (let col = 7; col < point.length; col++) {
      const sensorIdx = col - 6;
      const sl = sparklines.get(sensorIdx);
      if (sl) {
        const raw = point[col];
        sl.push(raw != null ? Number(raw) : NaN);
      }
    }
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

const updateFromStatus = (
  data: StatusPayload,
  sparklines: Map<number, Sparkline>,
  pidCharts: Map<number, PidChart>,
  setControlProbeIndex: (nextIndex: number | undefined) => void,
  setTempProbeLabel: (index: number, label: string | null | undefined) => void,
  setPidProbeLabel: (index: number, label: string | null | undefined) => void,
  ensureTemperatureCharts: (sensors: StatusPayload["sensors"]) => void,
): void => {
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
    const totalElapsed = loadedHistoryBaseSeconds + data.system.uptime_s;
    sparklines.forEach((sparkline) => {
      sparkline.setElapsedSeconds(totalElapsed);
    });
    pidCharts.forEach((pidChart) => {
      pidChart.setElapsedSeconds(totalElapsed);
    });
  }

  setText("title", `${data.device.toUpperCase()} CONTROL PANEL`);
  setText("updated", `Updated ${new Date().toLocaleTimeString()}`);
  if (data.system && data.system.uptime_s !== null) {
    setText("uptime", `Uptime: ${formatUptime(data.system.uptime_s)}`);
  }
  ensureTemperatureCharts(data.sensors || []);

  const primarySensor = data.sensors && data.sensors.length > 0 ? data.sensors[0] : null;
  if (primarySensor) {
    setText("temp", formatTemp(primarySensor.temperature_c, "C"));
    setText("temp-secondary", formatTemp(primarySensor.temperature_f, "F"));
  }

  if (Array.isArray(data.sensors)) {
    data.sensors.forEach((sensor) => {
      setTempProbeLabel(sensor.index, sensor.name);
      setPidProbeLabel(sensor.index, sensor.name);

      if (collecting && sensor.temperature_c !== null) {
        const sensorChart = sparklines.get(sensor.index);
        if (sensorChart) {
          sensorChart.push(sensor.temperature_c);
        }
      }
    });
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
    const sample: PidSample = {
      target_c: data.pid.target_c,
      kp: data.pid.kp,
      ki: data.pid.ki,
      kd: data.pid.kd,
      output_percent: data.pid.output_percent,
      window_step: data.pid.window_step,
      on_steps: data.pid.on_steps,
      relay_on: data.pid.relay_on ? 1 : 0,
    };
    pidCharts.forEach((pidChart) => {
      pidChart.push(sample);
    });
  }

  setText("ip", data.system.ip || "--");
  updateNtpPill(data.system.ntp.synced);
};

const loop = async (
  sparklines: Map<number, Sparkline>,
  pidCharts: Map<number, PidChart>,
  primaryPidChart: PidChart,
  setControlProbeIndex: (nextIndex: number | undefined) => void,
  setTempProbeLabel: (index: number, label: string | null | undefined) => void,
  setPidProbeLabel: (index: number, label: string | null | undefined) => void,
  ensureTemperatureCharts: (sensors: StatusPayload["sensors"]) => void,
): Promise<void> => {
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
    updateFromStatus(
      payload,
      sparklines,
      pidCharts,
      setControlProbeIndex,
      setTempProbeLabel,
      setPidProbeLabel,
      ensureTemperatureCharts,
    );
    if (collecting) {
      await mergeHistoryFromDevice(sparklines, primaryPidChart);
    }
  } catch (error) {
    const msg = error instanceof TypeError ? "No link — retrying…" : `Update failed: ${String(error)}`;
    setText("updated", msg);
    const pill = byId<HTMLElement>("ntp-pill");
    pill.className = "status-pill status-danger";
    pill.textContent = "Link error";
  } finally {
    pollRequestInFlight = false;
  }
};

const start = (): void => {
  const sparklines = new Map<number, Sparkline>();
  const pidCharts = new Map<number, PidChart>();
  const tempLabelEls = new Map<number, HTMLElement>();
  const pidLabelEls = new Map<number, HTMLElement>();
  let controlProbeIndex = 0;

  const primaryCanvas = document.getElementById("temp-chart-0") as HTMLCanvasElement | null;
  const primaryCard = primaryCanvas?.closest("article") as HTMLElement | null;

  const pidCanvas = document.getElementById("pid-chart-0") as HTMLCanvasElement | null;
  const pidCard = pidCanvas?.closest("article") as HTMLElement | null;
  const section = document.querySelector("section") as HTMLElement | null;

  if (!pidCanvas || !primaryCanvas || !primaryCard || !pidCard || !section) {
    setText("updated", "Error: Dashboard layout mismatch");
    return;
  }

  primaryCard.dataset.sensorIndex = "0";
  primaryCard.dataset.cardType = "temp";
  pidCard.dataset.sensorIndex = "0";
  pidCard.dataset.cardType = "pid";

  sparklines.set(0, new Sparkline(primaryCanvas));
  pidCharts.set(0, new PidChart(pidCanvas));

  const primaryChart = sparklines.get(0)!;
  const primaryPidChart = pidCharts.get(0)!;
  const primaryTempLabel = document.getElementById("temp-chart-probe-0") as HTMLElement | null;
  const primaryPidLabel = document.getElementById("pid-chart-probe-0") as HTMLElement | null;
  if (primaryTempLabel) {
    tempLabelEls.set(0, primaryTempLabel);
  }
  if (primaryPidLabel) {
    pidLabelEls.set(0, primaryPidLabel);
  }

  const setControlProbeIndex = (nextIndex: number | undefined): void => {
    if (!Number.isFinite(nextIndex)) {
      return;
    }
    const normalized = Math.max(0, Math.floor(nextIndex as number));
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

  const setLabelIfChanged = (el: HTMLElement | null | undefined, next: string): void => {
    if (!el) {
      return;
    }
    if (el.textContent !== next) {
      el.textContent = next;
    }
  };

  const setTempProbeLabel = (index: number, label: string | null | undefined): void => {
    const next = label || "--";
    const cached = tempLabelEls.get(index);
    if (cached) {
      setLabelIfChanged(cached, next);
      return;
    }
    const found = document.getElementById(`temp-chart-probe-${index}`) as HTMLElement | null;
    if (found) {
      tempLabelEls.set(index, found);
      setLabelIfChanged(found, next);
    }
  };

  const setPidProbeLabel = (index: number, label: string | null | undefined): void => {
    const next = label || "--";
    const cached = pidLabelEls.get(index);
    if (cached) {
      setLabelIfChanged(cached, next);
      return;
    }
    const found = document.getElementById(`pid-chart-probe-${index}`) as HTMLElement | null;
    if (found) {
      pidLabelEls.set(index, found);
      setLabelIfChanged(found, next);
    }
  };

  const hoverRatioForClientX = (canvas: HTMLCanvasElement, clientX: number): number => {
    const rect = canvas.getBoundingClientRect();
    const canvasX = ((clientX - rect.left) / rect.width) * canvas.width;
    const { axisPadLeft, sparklinePadRight } = CHART_LAYOUT;
    const plotWidth = Math.max(1, canvas.width - axisPadLeft - sparklinePadRight);
    return Math.max(0, Math.min(1, (canvasX - axisPadLeft) / plotWidth));
  };

  // Set up hover synchronization between primary temperature chart and PID chart
  primaryChart.canvas.addEventListener("mousemove", (event: MouseEvent) => {
    primaryPidChart.setHoverRatio(hoverRatioForClientX(primaryChart.canvas, event.clientX));
  });
  primaryChart.canvas.addEventListener("mouseleave", () => {
    primaryPidChart.setHoverRatio(null);
  });
  pidCanvas.addEventListener("mousemove", (event: MouseEvent) => {
    primaryChart.setHoverRatio(hoverRatioForClientX(pidCanvas, event.clientX));
  });
  pidCanvas.addEventListener("mouseleave", () => {
    primaryChart.setHoverRatio(null);
  });

  const applyZoom = (pivotRatio: number, factor: number): void => {
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

  const applyPan = (delta: number): void => {
    const span = zoomEnd - zoomStart;
    const newStart = Math.max(0, Math.min(1 - span, zoomStart + delta * span));
    setZoomWindow(newStart, newStart + span);
    sparklines.forEach((sparkline) => sparkline.redraw());
    pidCharts.forEach((chart) => chart.redraw());
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
    setZoomWindow(0, 1);
    sparklines.forEach((sparkline) => sparkline.redraw());
    pidCharts.forEach((chart) => chart.redraw());
  };

  section.addEventListener("wheel", (event: WheelEvent) => {
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

  section.addEventListener("dblclick", (event: MouseEvent) => {
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

  const createTempCard = (index: number, name: string | null | undefined): boolean => {
    const newTempCard = primaryCard.cloneNode(true) as HTMLElement;
    newTempCard.dataset.sensorIndex = String(index);
    newTempCard.dataset.cardType = "temp";

    const tempNameEl = newTempCard.querySelector(".chart-title-left") as HTMLElement | null;
    if (tempNameEl) {
      tempNameEl.id = `temp-chart-probe-${index}`;
      tempLabelEls.set(index, tempNameEl);
      setLabelIfChanged(tempNameEl, name || `probe-${index + 1}`);
    }

    const tempCanvas = newTempCard.querySelector("canvas.chart") as HTMLCanvasElement | null;
    if (!tempCanvas) {
      return false;
    }
    tempCanvas.id = `temp-chart-${index}`;
    tempCanvas.width = CHART_CANVAS_WIDTH;
    tempCanvas.height = CHART_CANVAS_HEIGHT;

    const sparkline = new Sparkline(tempCanvas);
    sparklines.set(index, sparkline);
    sparkline.setElapsedSeconds(loadedHistoryBaseSeconds);

    section.appendChild(newTempCard);
    return true;
  };

  const createPidCard = (index: number, name: string | null | undefined): boolean => {
    const newPidCard = pidCard.cloneNode(true) as HTMLElement;
    newPidCard.dataset.sensorIndex = String(index);
    newPidCard.dataset.cardType = "pid";

    const pidNameEl = newPidCard.querySelector(".chart-title-left") as HTMLElement | null;
    if (pidNameEl) {
      pidNameEl.id = `pid-chart-probe-${index}`;
      pidLabelEls.set(index, pidNameEl);
      setLabelIfChanged(pidNameEl, name || `probe-${index + 1}`);
    }

    const newPidCanvas = newPidCard.querySelector("canvas.chart") as HTMLCanvasElement | null;
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
    return true;
  };

  const ensureTemperatureCharts = (sensors: StatusPayload["sensors"]): void => {
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

    const tempCards: HTMLElement[] = [];
    section.querySelectorAll("article[data-card-type]").forEach((el) => {
      const article = el as HTMLElement;
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
  targetInput.addEventListener("keydown", (event: KeyboardEvent) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void applyTarget();
    }
  });

  loadHistoryFromDevice(sparklines, primaryPidChart)
    .catch((error) => {
      const msg = error instanceof TypeError ? "No link — retrying…" : `History load failed: ${String(error)}`;
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
  }, 5000);

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
        sparklines.forEach((sparkline) => sparkline.clear());
        pidCharts.forEach((chart) => chart.clear());
        lastUptimeSeconds = null;
        loadedHistoryBaseSeconds = 0;
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
