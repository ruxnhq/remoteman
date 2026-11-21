// remoteman.ruxn.com: remoteman's downloads page and privacy policy, as static files in public/,
// with no code of its own. Deployed from this folder with `cf deploy public`. The downloads
// themselves are on files.ruxn.com. A plain object, so the folder needs no packages installed.
export default {
  worker: {
    name: "remoteman-web",
    compatibilityDate: "2026-11-01",
    domains: ["remoteman.ruxn.com"],
    workersDev: false,
  },
};
