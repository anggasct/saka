import { platform } from "@/platform";

import "./styles.css";

const boot = async () => {
  await platform.ready();
  const { start } = await import("./app");
  await start();
};

void boot();
