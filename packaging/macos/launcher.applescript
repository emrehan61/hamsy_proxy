-- The installed binary path lives outside the bundle, so changing the
-- installation prefix never modifies a signed application.
on open harFiles
	my openHARFiles(harFiles)
end open

on run
	try
		set harFiles to choose file with prompt "Choose HAR files to open in Hamsy" with multiple selections allowed
		my openHARFiles(harFiles)
	on error errorMessage number errorNumber
		if errorNumber is not -128 then display alert "Hamsy could not open the HAR" message errorMessage
	end try
end run

on openHARFiles(harFiles)
	try
		set supportFolder to POSIX path of (path to application support from user domain)
		set configFile to POSIX file (supportFolder & "Hamsy/desktop/binary-path")
		set binaryPath to paragraph 1 of (read configFile as «class utf8»)
		set launchCommand to quoted form of binaryPath & " open --"
		repeat with harFile in harFiles
			set launchCommand to launchCommand & " " & quoted form of (POSIX path of harFile)
		end repeat
		do shell script launchCommand
	on error errorMessage
		display alert "Hamsy could not open the HAR" message (errorMessage & return & return & "Reinstall Hamsy to repair its launcher if the executable has moved.")
	end try
end openHARFiles
