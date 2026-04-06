export {};

type NullableNumber = number | null;

export const delayMs = (ms: number): Promise<void> => new Promise((resolve) => {
  window.setTimeout(resolve, ms);
});

export const byId = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) {
    throw new Error(`Missing element: ${id}`);
  }
  return el as T;
};

export const setText = (id: string, text: string): void => {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
};

export const formatTemp = (value: NullableNumber, unit: "C" | "F"): string => {
  if (value === null || Number.isNaN(value)) return `--.- ${unit}`;
  return `${value.toFixed(1)} ${unit}`;
};

export const formatNumber = (value: NullableNumber, suffix = ""): string => {
  if (value === null || Number.isNaN(value)) return "--";
  return `${value.toFixed(2)}${suffix}`;
};

export const formatElapsed = (totalSeconds: number): string => {
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

export const formatUptime = (uptimeSec: number): string => {
  const h = Math.floor(uptimeSec / 3600);
  const m = Math.floor((uptimeSec % 3600) / 60);
  const s = uptimeSec % 60;
  return `${h}h ${m}m ${s}s`;
};

export const setTargetFeedback = (text: string, tone: "normal" | "ok" | "error" = "normal"): void => {
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

export const updateNtpPill = (synced: boolean): void => {
  const pill = byId<HTMLElement>("ntp-pill");
  if (synced) {
    pill.className = "status-pill status-ok";
    pill.textContent = "NTP synced";
  } else {
    pill.className = "status-pill status-warn";
    pill.textContent = "NTP pending";
  }
};
