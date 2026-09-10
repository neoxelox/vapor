// Renders the two contact sheets under source/ from the current exports.
// Run from this directory after build-icons.cjs: `node make-previews.cjs`.
const fs=require('fs'),path=require('path'),sharp=require('sharp');const r=path.resolve(__dirname,'..','..');
const data=(p)=>fs.readFileSync(path.join(r,p)).toString('base64');
const img=(p,x,y,w,h=w)=>`<image href="data:image/${p.endsWith('.jpg')?'jpeg':'png'};base64,${data(p)}" x="${x}" y="${y}" width="${w}" height="${h}"/>`;
const text=(s,x,y,n=20,col='#242424')=>`<text x="${x}" y="${y}" font-family="sans-serif" font-size="${n}" fill="${col}">${s}</text>`;
(async()=>{
 let s=`<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1380"><defs><pattern id="check" width="24" height="24" patternUnits="userSpaceOnUse"><rect width="24" height="24" fill="#fff"/><path d="M0 0h12v12H0z M12 12h12v12H12z" fill="#e7e7e7"/></pattern></defs><rect width="1600" height="1380" fill="#f0efed"/>`;
 s+=text('Vapor · desktop icons',48,55,32);
 s+=text('Legacy fallback · 824 × 824 enclosure',48,105,21);
 s+='<rect x="48" y="130" width="470" height="470" fill="url(#check)"/>';
 s+=img('macos/Vapor.iconset/icon_512x512@2x.png',48,130,470);
 s+='<rect x="93.8984" y="175.8984" width="378.2031" height="378.2031" fill="none" stroke="#16a05d" stroke-width="1.5"/>';
 s+=text('No external shadow. Bounds: (100, 100)–(924, 924).',48,635,17);
 s+=text('Icon Composer · foreground layer',570,105,21);
 s+='<rect x="570" y="130" width="470" height="470" fill="url(#check)"/>';
 s+=img('macos/Vapor.icon/Assets/vapor-sculpted.png',570,130,470);
 s+=text('Unmasked 1024 canvas; macOS 26 supplies the enclosure.',570,635,17);
 s+=text('Background gradients',1090,105,21);
 s+=img('source/masters/background-light-1024.png',1090,140,180);
 s+=img('source/masters/background-dark-1024.png',1310,140,180);
 s+=text('Light / dark full bleed',1090,355,17);
 s+=text('Desktop export sizes',1090,420,21);
 let x=1090;for(const n of [128,64,32,16]){s+=img(`windows/png/vapor-${n}.png`,x,452,n);s+=text(n+' px',x,620,15);x+=n+20;}
 s+=text('Windows unplated · light and dark surfaces',48,715,21);
 s+='<rect x="48" y="742" width="350" height="155" rx="12" fill="white"/><rect x="418" y="742" width="350" height="155" rx="12" fill="#111820"/>';
 for(const bx of [74,444]){s+=img('windows/unplated/vapor-96.png',bx,765,96);s+=img('windows/unplated/vapor-48.png',bx+122,791,48);s+=img('windows/unplated/vapor-24.png',bx+208,803,24);s+=img('windows/unplated/vapor-16.png',bx+264,807,16);}
 s+=text('Transparent browser favicons',840,715,21);
 s+='<rect x="840" y="742" width="290" height="155" rx="12" fill="white"/><rect x="1150" y="742" width="350" height="155" rx="12" fill="#111820"/>';
 for(const bx of [875,1190]){s+=img('web/public/favicon-48x48.png',bx,790,48);s+=img('web/public/favicon-32x32.png',bx+80,798,32);s+=img('web/public/favicon-16x16.png',bx+150,806,16);}
 s+=text('README banners · the legacy tile composited into both themes',48,966,21);
 s+=img('github/vapor-banner-light.png',48,995,735,245);s+=img('github/vapor-banner-dark.png',815,995,735,245);
 s+=text('Asset previews, not screenshots of macOS, Windows, or Linux.',48,1320,18,'#555');s+='</svg>';
 await sharp(Buffer.from(s)).png().toFile(path.join(r,'source/desktop-icons-preview.png'));
 let g=`<svg xmlns="http://www.w3.org/2000/svg" width="1440" height="1050"><rect width="1440" height="1050" fill="#f0efed"/>${text('Vapor · GitHub and web',40,55,28)}`;
 g+=img('github/vapor-banner-light.png',0,90,720,240)+img('github/vapor-banner-dark.png',720,90,720,240);
 g+=text('GitHub social previews · 1280 × 640',40,385,22)+img('github/vapor-social-light-1280x640.jpg',40,420,640,320)+img('github/vapor-social-dark-1280x640.jpg',760,420,640,320);
 g+=text('Web Open Graph: 1200 × 630. Large cards: 1200 × 600. Theme markup in the root README.',40,820,20);g+='</svg>';
 await sharp(Buffer.from(g)).png().toFile(path.join(r,'source/github-and-web-preview.png'));
 console.log('Rendered source/desktop-icons-preview.png and source/github-and-web-preview.png');
})();
