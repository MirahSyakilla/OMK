import { exec } from 'kernelsu-alt'

let hour12: boolean | undefined
let hour12Loaded = false

async function deviceHour12(): Promise<boolean | undefined> {
  if (hour12Loaded) return hour12
  hour12Loaded = true
  if (import.meta.env.DEV) return undefined
  const result = await exec('settings get system time_12_24')
  const value = result.stdout.trim()
  if (value === '12') hour12 = true
  else if (value === '24') hour12 = false
  return hour12
}

export async function formatDeviceDate(date: Date, withTime = false): Promise<string> {
  const options: Intl.DateTimeFormatOptions = { dateStyle: 'medium' }
  if (withTime) {
    options.timeStyle = 'short'
    const useHour12 = await deviceHour12()
    if (useHour12 !== undefined) options.hour12 = useHour12
  }
  return new Intl.DateTimeFormat(undefined, options).format(date)
}
