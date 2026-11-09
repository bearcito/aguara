// Datos de prueba para ver la interfaz en un navegador (npm run dev) sin la app de escritorio
// ni teléfonos. En el build de producción `import.meta.env.DEV` es false y esto desaparece.
if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
  const devices = [
    { serial: "demo-01", state: "device", model: "Redmi 25028RN03A" },
    { serial: "demo-02", state: "device", model: "Redmi 25028RN03A" },
    { serial: "demo-03", state: "device", model: "Un teléfono con un nombre larguísimo de prueba" },
    { serial: "demo-04", state: "unauthorized", model: null },
  ];
  const details = {
    brand: "Redmi",
    model: "25028RN03A",
    android: "15",
    battery_level: 23,
    charging: false,
    storage_total_kb: 118_000_000,
    storage_free_kb: 9_400_000,
    ram_total_kb: 7_800_000,
    ram_available_kb: 2_100_000,
    screen_on: true,
    ip: "192.168.0.47",
    accounts: [
      { kind: "www.instagram.com", name: "cuenta.demo_1" },
      { kind: "www.instagram.com", name: "cuenta.demo_2" },
      { kind: "com.facebook.auth.login", name: "100094812345678" },
      { kind: "com.google", name: "demo@example.com" },
    ],
    packages: ["com.instagram.android", "com.facebook.katana", "com.zhiliaoapp.musically", "com.whatsapp"],
  };
  const replies: Record<string, unknown> = {
    list_devices: location.search.includes("empty") ? [] : devices,
    list_active: [],
    list_audio: [],
    device_details: details,
    panic_wipe: 12,
    foreign_adb: location.search.includes("foreign") ? [{ port: 5038, devices: 4 }] : [],
    "plugin:app|version": "dev",
  };
  let cb = 0;
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    transformCallback: () => ++cb,
    invoke: async (cmd: string) => {
      // ?slow simula una primera búsqueda lenta (para ver el logo y los esqueletos).
      if (cmd === "list_devices" && location.search.includes("slow") && performance.now() < 8000) {
        await new Promise((r) => setTimeout(r, 4000));
      }
      // ?connecting deja los teléfonos conectando (para ver su esqueleto).
      if (cmd === "start_mirror" && location.search.includes("connecting")) return new Promise(() => {});
      if (cmd in replies) return replies[cmd];
      if (cmd.startsWith("plugin:event")) return 0;
      if (cmd === "start_mirror") throw new Error("demo: sin teléfonos reales");
      return null;
    },
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  };
}
