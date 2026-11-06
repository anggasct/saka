import { platform } from "@/platform";

import "./styles.css";

void platform.ready().then(async () => {
  const { start } = await import("./app");
  await start();
});
