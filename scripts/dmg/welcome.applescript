-- Opens Terminal and starts a Tap recording there (`tap launch`: a first-run check, then a recorded
-- shell; on exit the recording opens in the player). The CLI path arrives as an
-- argument and is shell-quoted here, so app locations containing spaces or quotes work.
on welcomeCommand(tapPath)
	set tapCommand to quoted form of tapPath
	return tapCommand & " launch"
end welcomeCommand

on run argv
	set command to welcomeCommand(item 1 of argv)
	tell application "Terminal"
		activate
		do script command
	end tell
end run
