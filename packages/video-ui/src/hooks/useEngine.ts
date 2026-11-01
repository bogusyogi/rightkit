import { useEffect, useRef, useState } from "react";
import {
  type DoctorReport,
  type EngineStatus,
  engineDoctor,
  engineStatus,
  errorMessage,
} from "../lib/engine";

const POLL_MS = 1000;

/** Latest doctor report, read by views that need one readiness check without polling. */
let latestDoctor: DoctorReport | null = null;
const doctorListeners = new Set<() => void>();

export function subscribeDoctor(listener: () => void): () => void {
  doctorListeners.add(listener);
  return () => {
    doctorListeners.delete(listener);
  };
}

export const getDoctor = () => latestDoctor;

function publishDoctor(report: DoctorReport) {
  latestDoctor = report;
  doctorListeners.forEach((listener) => listener());
}

/** Polls `engine_status` and loads the doctor report once the engine is ready. */
export function useEngine() {
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [doctor, setDoctor] = useState<DoctorReport | null>(null);
  const [doctorError, setDoctorError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const doctorRequested = useRef(false);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const next = await engineStatus();
        if (cancelled) return;
        setStatus(next);
        setStatusError(null);
        if (next.state !== "ready") {
          doctorRequested.current = false;
        } else if (!doctorRequested.current) {
          doctorRequested.current = true;
          engineDoctor()
            .then((report) => {
              if (cancelled) return;
              setDoctor(report);
              publishDoctor(report);
              setDoctorError(null);
            })
            .catch((error) => {
              if (cancelled) return;
              doctorRequested.current = false;
              setDoctorError(errorMessage(error));
            });
        }
      } catch (error) {
        if (!cancelled) setStatusError(errorMessage(error));
      }
    };
    void tick();
    const timer = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  return { status, doctor, doctorError, statusError };
}
