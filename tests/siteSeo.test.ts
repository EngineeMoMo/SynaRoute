import { describe, expect, it } from "vitest";
import { normalizeSitePath } from "../site/src/lib/seoPath";

describe("site canonical paths", () => {
  it.each([
    ["/", "/"],
    ["/index.html", "/"],
    ["/en", "/en"],
    ["/en/", "/en"],
    ["/en.html", "/en"],
    ["/en/index.html", "/en"],
    ["/zh/docs/cli/", "/zh/docs/cli"],
    ["/zh/docs/cli.html", "/zh/docs/cli"],
    ["/zh/docs/cli/index.html", "/zh/docs/cli"],
  ])("renders %s under the same canonical route as %s", (pathname, expected) => {
    expect(normalizeSitePath(pathname)).toBe(expected);
  });

  it.each(["/missing", "/de/docs", "/en/docs/missing"])(
    "keeps unknown route %s available for the not-found page",
    (pathname) => expect(normalizeSitePath(pathname)).toBe(pathname),
  );
});
