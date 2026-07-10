// Ambient type for text-imported `.sql` files (`import ddl from "./x.sql" with
// { type: "text" }`). Bun's bundler + `bun build --compile` inline the file's
// content as a string (verified); this declaration gives TypeScript the matching
// `string` default export so the embedded migrator (db/embedded-migrations.ts)
// typechecks. Not a runtime file — types only.
declare module "*.sql" {
  const content: string;
  export default content;
}
