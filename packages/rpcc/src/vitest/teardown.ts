import { runRestore } from './setup.js'

export default async function globalTeardown() {
  runRestore()
}
