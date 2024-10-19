Must
- [x] configure group tag for all resources created by this tool
- [ ] global teardown of all resources created by this tool
- [ ] better hardware selection flow
- [ ] display alternative UI in non-debug mode

- [ ] auth login

- [x] support stdin (eg. -y prompts to be received from opened session)
- [x] shell subcommand to SSH into remote instance 
- [x] respect .gitignore during syncing/upload

Nice-to-have
- [ ] show current billing
- [ ] show pricing in select instance type options?
- [ ] auto stop/terminate
- [x] cargo remote run/r

- [ ] parse toml/yaml config to specify shortcut
- [ ] cargo remote check/c (specific to rust)
- [ ] cargo remote test/t

- [x] way to "tunnel" into local service running in remote server

1. dst must be a DIR that has been created

    Eg. You must specify a directory that exists. Otherwise you have to create directories.
    abc
    abc/def

2. No dst specified, default dst = $HOME/root => create root folder (except if the root is $HOME / upload from $HOME)

    Eg. Files within CWD uploaded to $HOME/root OR [dst] folder
    ./bin upload [dst]
    ./bin upload output.txt [dst]
    ./bin upload src/main.rs [dst]
    ./bin upload src/dir1/test.rs [dst]

    Eg. Files/folders within $HOME dir
    ./bin upload $HOME/foobar.txt # no root folder is created
    ./bin upload $HOME/abc/foobar.txt

3. recreate any src folders needed

    ./bin upload src/test.txt # folder src is created
    ./bin upload src/abc/test.txt # folders src, abc are created

4. src = (some path outside root folder)

    ./bin upload ../test.txt
    ./bin upload ../src/test.txt # src is created

    Files/folders will be uploaded under $HOME (if dst is not specified)
    Files/folders will be uploaded under dst folder 
