import init, {
  connect,
  stop,
  set_volume,
  set_status_callback,
  set_stats_callback,
} from "./pkg/wasm_client.js";

const form = document.getElementById("connect-form");
const urlInput = document.getElementById("server-url");
const button = document.getElementById("connect-btn");
const statusEl = document.getElementById("status");
const statsEl = document.getElementById("stats");
const volume = document.getElementById("volume");

let connected = false;

function defaultServerUrl() {
  const scheme = location.protocol === "https:" ? "wss://" : "ws://";
  return `${scheme}${location.host}/ws`;
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!connected) {
    await init();
    const url = urlInput.value.trim() || defaultServerUrl();
    connect(url, "./worklet.js");
    button.textContent = "Stop";
    connected = true;
  } else {
    stop();
    button.textContent = "Connect";
    connected = false;
  }
});

set_status_callback((message) => {
  statusEl.textContent = message;
});

set_stats_callback((packets, bytes) => {
  statsEl.textContent = `${packets} packets · ${(bytes / 1024).toFixed(0)} KiB`;
});

volume.addEventListener("input", () => {
  set_volume(parseFloat(volume.value));
});
set_volume(parseFloat(volume.value));
