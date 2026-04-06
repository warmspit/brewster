export {};

export type NullableNumber = number | null;

export type SensorReading = {
  index: number;
  name: string;
  temperature_c: NullableNumber;
  temperature_f: NullableNumber;
  error: string;
};

export type StatusPayload = {
  device: string;
  hostname?: string;
  control_probe_index?: number;
  sensors: SensorReading[];
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
    heat_on?: boolean;
  };
  system: {
    ip: string;
    device_http_port: number;
    collecting?: boolean;
    seq: number;
    packets_dropped: number;
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

export type PidSample = {
  target_c: number;
  kp: number;
  ki: number;
  kd: number;
  output_percent: number;
  window_step: number;
  on_steps: number;
  relay_on: number;
};

export type HistoryPayload = {
  sample_interval_s: number;
  total_samples: number;
  points: number[][];
};

export const HISTORY_FETCH_POINTS = 2000;

export const submitTargetTemperature = async (tempC: number): Promise<void> => {
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

export const clearHistoryOnDevice = async (): Promise<void> => {
  const response = await fetch("/history/clear", { method: "POST" });
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
};

export const setCollectionOnDevice = async (enabled: boolean, getPollInFlight: () => boolean): Promise<void> => {
  const path = enabled ? "/collection/start" : "/collection/stop";
  // Avoid colliding control requests with in-flight polling on the single-connection HTTP task.
  for (let i = 0; i < 6 && getPollInFlight(); i += 1) {
    await new Promise((resolve) => { window.setTimeout(resolve, 80); });
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
        await new Promise((resolve) => { window.setTimeout(resolve, 120 * attempt); });
      }
    }
  }
  throw lastError instanceof Error ? lastError : new Error(String(lastError));
};
