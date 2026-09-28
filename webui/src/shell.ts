/**
 * Shell quoting for values that reach a command string.
 *
 * `exec()` hands its argument to a shell, which parses the whole string once.
 * A double-quoted region therefore ends at the first `"` in the value, and
 * whatever follows is parsed as shell syntax: a `;` becomes a command
 * separator, a backtick a substitution, `$(...)` an expansion. So a value
 * containing a double quote is a command injection even without any second
 * pass, `eval`, or `sh -c` involved.
 *
 * That matters because paths here come from a directory listing of
 * `/storage/emulated/0/Download`, which any unprivileged app with storage
 * access can write to, and Android only forbids `/` and NUL in a filename. A
 * file called `x"; id; echo "` is a legal name and a working payload.
 *
 * Wrapping in single quotes is the fix: nothing inside single quotes is
 * special to the shell, so the only character needing care is `'` itself,
 * which is closed, escaped, and reopened.
 */
export function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`
}

/**
 * Whether a filename from a directory listing is safe to interpolate anywhere.
 *
 * Defence in depth on top of {@link shellQuote}. The listing no longer has to
 * be escaped to be safe, and rejecting these outright means a name that is
 * somehow re-expanded later cannot become a command.
 */
export function isSafeFilename(name: string): boolean {
  if (!name || name === '.' || name === '..') return false
  // Reject anything with a newline, which would break the line-oriented
  // `d|<name>` / `f|<name>` protocol the listing parser relies on.
  return !/["'`$\\;|&<>*?\n\r\t]/.test(name)
}
