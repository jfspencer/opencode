// The reference's `Npm.which(spec)` in a fresh process (the environment says where the caches and config are);
// prints `WHICH:<bin or undefined>`.
//
// usage: NPM_REFERENCE=<port/.reference> bun which.ts <spec>
const reference = process.env.NPM_REFERENCE!
const { Effect } = await import(`${reference}/packages/core/node_modules/effect/dist/index.js`)
const { AppNodeBuilder } = await import(`${reference}/packages/core/src/effect/app-node-builder.ts`)
const { Npm } = await import(`${reference}/packages/util/src/npm.ts`)
const bin = await Effect.runPromise(
  Effect.provide(
    Effect.gen(function* () {
      const npm = yield* Npm.Service
      return yield* npm.which(process.argv[2])
    }),
    AppNodeBuilder.build(Npm.node),
  ),
)
console.log(`WHICH:${bin ?? "undefined"}`)
