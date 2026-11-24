import { isWindows, type HealthReading } from "./api.js";

/** Warning as the notch counts it: failed self-assessment, a critical warning, or media errors. */
export function isWarning(reading: HealthReading): boolean {
  return reading.passed === false || (reading.critical_warning ?? 0) !== 0 || (reading.media_errors ?? 0) > 0;
}

/** Windows only: the drive answered, but not one metric came with it (the unelevated paths gave nothing). */
export function lacksMetrics(reading: HealthReading): boolean {
  return (
    isWindows &&
    reading.temperature_c == null &&
    reading.wear_percent == null &&
    reading.written_bytes == null &&
    reading.power_on_hours == null &&
    reading.media_errors == null
  );
}

export function healthSeries(history: HealthReading[], pick: (r: HealthReading) => number | null) {
  return history.flatMap((r) => {
    const value = pick(r);
    return value == null ? [] : [{ at: r.at, value }];
  });
}
