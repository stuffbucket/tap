-- Opens Terminal and runs Tap's first-run check and quickstart. The CLI path arrives as an
-- argument and is shell-quoted here, so app locations containing spaces or quotes work.
on welcomeCommand(tapPath)
	set tapCommand to quoted form of tapPath
	return tapCommand & " doctor; echo; " & tapCommand & " quickstart"
end welcomeCommand

on run argv
	set command to welcomeCommand(item 1 of argv)
	tell application "Terminal"
		activate
		do script command
	end tell
end run
