import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const siteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const suffixes = {
  benefits: ["name", "desc", "more"],
  features: ["name", "short", "desc"],
  steps: ["title", "desc"],
  screenshots: ["title", "desc"],
  security: ["title", "desc"],
  docs: ["title", "desc"],
  brain: ["title", "desc"],
};

export function checkTranslations(sources, dictionaries) {
  const references = new Set();
  const errors = [];
  for (const [filename, source] of Object.entries(sources)) {
    const tree = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const visit = (node) => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === "t") {
        const argument = node.arguments[0];
        if (argument && (ts.isStringLiteral(argument) || ts.isNoSubstitutionTemplateLiteral(argument))) {
          references.add(argument.text);
        }
      }
      if (ts.isPropertyAssignment(node) && ts.isStringLiteral(node.initializer)) {
        const name = node.name.getText(tree).replace(/["']/g, "");
        const value = node.initializer.text;
        if (/^(label|title|question|answer)Key$/.test(name)) references.add(value);
        if (name === "i18nPrefix" || (name === "key" && value.startsWith("brain.cap."))) {
          const endings = suffixes[value.split(".")[0]];
          if (!endings) errors.push(filename + ": unhandled translation prefix " + value);
          for (const ending of endings ?? []) references.add(value + "." + ending);
        }
        if (filename.endsWith("platforms.ts") && name === "id") {
          for (const ending of ["name", "format"]) references.add("platform." + value + "." + ending);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(tree);
  }
  const allKeys = new Set(Object.values(dictionaries).flatMap(dictionary => Object.keys(dictionary)));
  for (const key of [...references, ...allKeys]) {
    for (const [language, dictionary] of Object.entries(dictionaries)) {
      if (!Object.hasOwn(dictionary, key) || !dictionary[key]?.trim()) errors.push(language + ": missing " + key);
    }
  }
  return [...new Set(errors)];
}

function collectSources(directory) {
  return Object.fromEntries(fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) return Object.entries(collectSources(filename));
    return /\.tsx?$/.test(filename) ? [[filename, fs.readFileSync(filename, "utf8")]] : [];
  }));
}

function readDictionary(language) {
  const source = fs.readFileSync(path.join(siteRoot, "src/i18n", language + ".ts"), "utf8");
  const tree = ts.createSourceFile(language + ".ts", source, ts.ScriptTarget.Latest, true);
  const dictionary = {};
  const textValue = node => {
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
    if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
      return textValue(node.left) + textValue(node.right);
    }
    throw new Error(language + ": unsupported dictionary value " + node.getText(tree));
  };
  const visit = node => {
    if (ts.isPropertyAssignment(node) && ts.isStringLiteral(node.name)) {
      dictionary[node.name.text] = textValue(node.initializer);
    }
    ts.forEachChild(node, visit);
  };
  visit(tree);
  return dictionary;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const errors = checkTranslations(collectSources(path.join(siteRoot, "src")), {
    zh: readDictionary("zh"), en: readDictionary("en"),
  });
  if (errors.length) {
    console.error(errors.join("\n"));
    process.exitCode = 1;
  } else console.log("Site translation references and language parity: OK");
}
