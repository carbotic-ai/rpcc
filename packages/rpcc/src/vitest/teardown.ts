import { runRestore } from './setup'

export default async function globalTeardown() {
  runRestore()
}
