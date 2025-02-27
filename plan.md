# config file
- proxy address;
- concurrent downloading number;

# design
refactor how to download:
1. split the file downloading task into multiple;
2. calculate the speed via/not via proxy, then use/not use proxy depending on the better speed;
3. Do these tasks seperately using Tokio;