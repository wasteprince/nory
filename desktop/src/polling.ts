// One in-flight update; hidden windows do no UI polling. VPN supervision lives
// in Rust and continues while the WebView is hidden or minimized.
export function visibilityPoller(update: () => Promise<void>, delay: () => number) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false, visible = true, inFlight = false;
  function schedule(ms: number) {
    clearTimeout(timer);
    if (running && visible && !inFlight) timer = setTimeout(tick, ms);
  }
  async function tick() {
    if (!running || !visible || inFlight) return;
    inFlight = true;
    try { await update(); } catch { /* The next visible tick retries. */ }
    finally {
      inFlight = false;
      schedule(delay());
    }
  }
  return {
    start() { running = true; schedule(0); },
    setVisible(value: boolean) {
      if (visible === value) return;
      visible = value;
      schedule(0);
    },
    stop() { running = false; clearTimeout(timer); },
  };
}
