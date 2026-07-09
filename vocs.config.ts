import { defineConfig } from "vocs";

export default defineConfig({
  title: "imgx",
  description:
    "Fast, single-binary image proxy and transformation server. A self-hosted drop-in replacement for Cloudflare Images.",
  logoUrl: {
    light: "/logo-black.svg",
    dark: "/logo.svg",
  },
  rootDir: "docs",
  topNav: [
    {
      text: "Guide",
      link: "/getting-started",
      match: "/getting-started",
    },
    {
      text: "Reference",
      link: "/transforms",
      match: "/transforms",
    },
    {
      text: "GitHub",
      link: "https://github.com/officialunofficial/imgx",
    },
  ],
  sidebar: [
    {
      text: "Introduction",
      items: [
        { text: "Overview", link: "/" },
        { text: "Getting Started", link: "/getting-started" },
        {
          text: "Migrating from Cloudflare",
          link: "/migrating-from-cloudflare",
        },
      ],
    },
    {
      text: "Reference",
      items: [
        { text: "Transform Parameters", link: "/transforms" },
        { text: "Configuration", link: "/configuration" },
      ],
    },
    {
      text: "Operations",
      items: [
        { text: "Deployment", link: "/deployment" },
        { text: "Architecture", link: "/architecture" },
        { text: "Performance", link: "/performance" },
      ],
    },
  ],
  socials: [
    {
      icon: "github",
      link: "https://github.com/officialunofficial/imgx",
    },
  ],
  theme: {
    accentColor: "#0046FF",
  },
});
