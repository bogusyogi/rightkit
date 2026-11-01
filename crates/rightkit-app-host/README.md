# rightkit-app-host

Host services a desktop app gives the RightKit companion crates moved out of Pulse (chat bridge, AI accounts, agent gateway, approvals, web session): its state folder, the account home, its product name and what its UI last published. The app calls `install` once at start; until then the crates see a neutral host with no state folder.
