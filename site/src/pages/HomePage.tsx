import { Hero } from "@/components/sections/Hero";
import { Benefits } from "@/components/sections/Benefits";
import { Features } from "@/components/sections/Features";
import { Screenshots } from "@/components/sections/Screenshots";
import { Downloads } from "@/components/sections/Downloads";
import { GettingStarted } from "@/components/sections/GettingStarted";
import { Security } from "@/components/sections/Security";
import { FAQ } from "@/components/sections/FAQ";
import { FinalCTA } from "@/components/sections/FinalCTA";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import { siteConfig } from "@/config/site";

export default function HomePage() {
  const t = useT();
  const lang = useLang();

  useSeo({
    title: siteConfig.name,
    description: t("hero.desc"),
    lang,
  });

  return (
    <>
      <Hero />
      <Benefits />
      <Features />
      <Screenshots />
      <Downloads />
      <GettingStarted />
      <Security />
      <FAQ />
      <FinalCTA />
    </>
  );
}
