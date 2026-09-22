import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

declare const Deno: {
  readDirSync(path: string): Iterable<{
    name: string;
    isDirectory: boolean;
    isFile: boolean;
  }>;
  readFileSync(path: string): Uint8Array;
  writeTextFileSync(path: string, data: string): void;
};

const sourceFiles = [
  "index.html",
  "package.json",
  "deno.json",
  "deno.lock",
  "tsconfig.json",
  "vite.config.ts",
];

function collectSource(directory: string, files: string[]) {
  for (
    const entry of [...Deno.readDirSync(directory)].sort((left, right) =>
      left.name.localeCompare(right.name)
    )
  ) {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory) collectSource(path, files);
    else if (entry.isFile) files.push(path);
  }
}

function sourceIdentity() {
  const files = [...sourceFiles];
  collectSource("src", files);
  files.sort();
  let hash = 0xcbf29ce484222325n;
  for (const path of files) {
    for (const byte of new TextEncoder().encode(path)) {
      hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
    }
    for (const byte of Deno.readFileSync(path)) {
      hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
    }
  }
  return `fnv1a64-${hash.toString(16).padStart(16, "0")}`;
}

export default defineConfig({
  plugins: [
    react(),
    {
      name: "agenticjira-source-identity",
      writeBundle() {
        Deno.writeTextFileSync(
          "dist/.agenticjira-source-id",
          `${sourceIdentity()}\n`,
        );
      },
    },
  ],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    host: "127.0.0.1",
  },
});
